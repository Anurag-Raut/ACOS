//! The `acos_help` self-description.
//!
//! The prose comes from [`docs/AGENT_GUIDE.md`](../../../docs/AGENT_GUIDE.md),
//! embedded with `include_str!` so the installed binary carries the full guide
//! with no files to find at runtime. The guide is written for agents: the loop,
//! every CLI command with a runnable example, how to write a scenario, the
//! settle rules, error codes and their next action, and the limits.
//!
//! The payload also reports what is supported *right now* from the live
//! capability probe (never hard-coded) and folds in the summary lines of
//! `COMPATIBILITY.md` when that file is readable.

use std::path::Path;

use acos_protocol::Fidelity;
use serde_json::{json, Value};

use crate::backend::{BackendInfo, Capabilities};
use crate::descriptors::{command_fidelity, tool_infos, ToolInfo};

/// The agent guide, embedded at build time from `docs/AGENT_GUIDE.md`.
///
/// One source of truth: the file is the documentation for humans and the text
/// `acos_help` and the MCP resources return. A test asserts the embedded copy
/// and the file on disk cannot drift.
pub const AGENT_GUIDE: &str = include_str!("../../../docs/AGENT_GUIDE.md");

/// The guide topics `acos_help {"topic": ...}` accepts. Each names a
/// `<!-- topic:NAME -->` … `<!-- /topic:NAME -->` block in the guide.
///
/// `overview` is the skeleton `acos_help` returns with no topic; `all` is the
/// whole guide (not a section, so it is not in this list).
pub const TOPICS: &[&str] = &[
    "overview",
    "workflow",
    "cli",
    "scenarios",
    "errors",
    "limits",
];

/// `topic: "all"` returns everything.
pub const ALL_TOPIC: &str = "all";

/// The MCP cheat-sheet topic.
///
/// Unlike every other topic it is not a section of `docs/AGENT_GUIDE.md`: it is
/// **generated from the live tool descriptors** on every reply (see
/// [`mcp_topic`]), because the advertised tool set changes with the build. A
/// hand-written tool table would drift the day a tool is added, renamed or
/// hidden again — and an agent that trusts a stale table wastes a round trip per
/// wrong call.
pub const MCP_TOPIC: &str = "mcp";

/// Topics that are generated from code instead of sliced out of the guide.
pub const GENERATED_TOPICS: &[&str] = &[MCP_TOPIC];

/// Every topic name `acos_help {"topic": ...}` accepts, guide sections first.
#[must_use]
pub fn topic_list() -> Vec<&'static str> {
    let mut topics: Vec<&'static str> = TOPICS.to_vec();
    topics.extend_from_slice(GENERATED_TOPICS);
    topics
}

/// Is `topic` one this build serves?
#[must_use]
pub fn is_topic(topic: &str) -> bool {
    topic == ALL_TOPIC || topic_list().contains(&topic)
}

/// The marker `docs/AGENT_GUIDE.md` carries where the generated module table
/// belongs.
///
/// The guide is hand-written prose; the module table is not. Every module, its
/// profiles and every gap name come from the service-module registry
/// (ADR-0029 item 6), so a module that exists but is missing from the docs is
/// impossible by construction, and a doc that promises a module the build does
/// not ship is impossible too.
pub const MODULES_MARKER: &str = "<!-- modules:generated -->";

/// The generated limits appendix: every module and every gap name, from the
/// registry.
#[must_use]
pub fn generated_limits() -> String {
    acos_services::module::ModuleRegistry::standard().limits_markdown()
}

/// Render the guide, replacing [`MODULES_MARKER`] with the generated module
/// table.
#[must_use]
pub fn render_guide(text: &str) -> String {
    text.replace(MODULES_MARKER, &generated_limits())
}

/// The whole guide, trimmed, with the generated module table spliced in.
#[must_use]
pub fn full_guide() -> String {
    render_guide(AGENT_GUIDE.trim())
}

/// The skeleton `acos_help` returns with **no topic**: the guide's own front
/// section (`<!-- topic:overview -->`, which carries the three status rules,
/// the loop in ten lines and the topic list with one-line descriptions).
///
/// Why: the whole guide is ~6.4k tokens, and a client that truncated it
/// mid-scenario taught the agent a half-product. An agent that wants the whole
/// text asks for `topic: "all"`; one that wants a section asks for it by name.
#[must_use]
pub fn skeleton() -> String {
    let full = AGENT_GUIDE;
    let start = "<!-- topic:overview -->";
    let end = "<!-- /topic:overview -->";
    let preamble = full
        .find("<!-- topic:")
        .map_or("", |idx| &full[..idx])
        .trim();
    let section = match (full.find(start), full.find(end)) {
        (Some(from), Some(to)) if to > from => full[from + start.len()..to].trim(),
        _ => "",
    };
    format!("{preamble}\n\n{section}\n").trim().to_string()
}

/// A guide topic, the skeleton, or (with `topic: "all"`) the whole guide.
///
/// An unknown topic returns the skeleton — a wrong topic must not flood the
/// agent with the whole document, and it must not return nothing either.
#[must_use]
pub fn guide(topic: Option<&str>) -> String {
    match topic {
        None => skeleton(),
        Some(ALL_TOPIC) => full_guide(),
        // The generated topics need live descriptors; without a capability
        // snapshot only the always-available server tools can be advertised,
        // which would be a *wrong* table. Callers that have a snapshot use
        // [`guide_with`].
        Some(MCP_TOPIC) => mcp_topic(&tool_infos(&Capabilities::unprobed())),
        Some(topic) if TOPICS.contains(&topic) => section(topic).unwrap_or_else(skeleton),
        Some(_) => skeleton(),
    }
}

/// [`guide`] for a request the server answered with a live capability snapshot:
/// the generated topics (`mcp`) are built from the advertised tool set.
#[must_use]
pub fn guide_with(topic: Option<&str>, infos: &[ToolInfo]) -> String {
    match topic {
        Some(MCP_TOPIC) => mcp_topic(infos),
        other => guide(other),
    }
}

/// The `mcp` topic: a table of **every advertised MCP tool** -> required and
/// optional arguments -> one example call -> what it returns, plus the recipes
/// that used to be impossible to answer from the tool list alone.
///
/// Generated: the rows come from the same descriptors `tools/list` serves, so a
/// tool that is added, renamed, gains an argument or is hidden appears (or
/// disappears) here in the same build. Tools that are not advertised are not
/// listed at all — `acos_help` must not teach a call that returns
/// `unsupported`.
#[must_use]
pub fn mcp_topic(infos: &[ToolInfo]) -> String {
    let mut out = String::new();
    out.push_str("## MCP tools (this build, generated from the live tool list)\n\n");
    out.push_str(
        "Each row is a tool `tools/list` advertises right now: the arguments it takes, one call \
         you can paste, and what comes back. `session_id` defaults to this server's own session \
         (`mcp-<pid>`), so a single-agent loop never has to pass one. Tools that are NOT in this \
         table are not implemented by this build and are not advertised; a call to one still \
         returns a structured `unsupported` rather than a fake pass.\n\n",
    );
    out.push_str("| tool | required | optional | example | returns |\n");
    out.push_str("|---|---|---|---|---|\n");
    for tool in infos.iter().filter(|tool| tool.available) {
        let required = required_args(&tool.input_schema);
        let optional = optional_args(&tool.input_schema);
        out.push_str(&format!(
            "| `{}` | {} | {} | `{}` | {} |\n",
            tool.tool_name,
            list_cell(&required),
            list_cell(&optional),
            example_call(tool),
            tool_returns(tool),
        ));
    }
    out.push_str(
        "\n### Recipes\n\n\
         * **Take a screenshot.** Pass `screenshot: true` to `acos_observe_tree` (recommended — \
         captures a lossless PNG of the exact bound frame under `<artifacts_dir>/screenshots/` \
         and returns `screenshot.path`, `width`, `height`, `display_size`, `display_origin`, plus \
         an inline preview) or to `acos_observe`. A frame with no app pixels (such as a launch \
         placeholder) returns `NO_FRESH_FRAME`. Alternatives: a `capture: {name: shot, format: png, \
         fresh: true}` step in an `acos_verify` scenario (`frames/<seq>.png`), or on the CLI \
         `acos observe --screenshot out/shot.png --json`.\n\
         * **Type into a field.** `acos_observe_tree` -> `acos_tap {\"id\": \"...\"}` or \
         `{\"text\": \"...\"}` on the field -> `acos_text {\"text\": \"Ada\"}` -> `acos_observe_tree` \
         again to read the value. Type goes to the *focused* editor through the real \
         `InputConnection`; if the reply is `ELEMENT_NOT_FOUND` with `details.kind` \
         `NoFocusedEditor`, the tap did not land on an editable field. Live-session typing is the \
         one step that is still a gap on some builds — if `acos_text` answers \
         `UNSUPPORTED_BACKEND`, put the same interaction in a scenario `type:` step and run \
         `acos_verify`, which uses the same channel family and does work.\n\
         * **Find a control by `content_desc`.** `acos_observe_tree` returns every node with \
         `text` *and* `content_desc`; an icon button usually has an empty `text` and its name in \
         `content_desc` (for example `\"Search Japanese dictionary\"`). Select by it in a scenario \
         with `selector: {content_desc: {contains: Search}}`, and tap by handing the label to \
         `acos_tap {\"text\": ...}` (text or content-desc) or by computing the centre of the node's \
         `bounds` and tapping `x`/`y`. Never guess a coordinate from a screenshot: PNG space is \
         offset by `frame.display_origin`.\n\
         * **Prove a flow.** Write a YAML scenario (`acos_help {\"topic\": \"scenarios\"}) and run \
         `acos_verify {\"apk\": ..., \"scenario\": ...}` (real ART by default). Read \
         `verdict.json`, the per-step JSON and the referenced frames: a `pass` counts only with its \
         evidence on disk, and `expect_change: true` on the taps whose point is to change the screen \
         is what makes the pass mean something.\n\
         * **Assert, do not eyeball.** `assert_text` with `bounds`, `assert_visible` / \
         `assert_not_visible` (collapsed content stays in the tree with `visible: false`) and \
         `assert_log` turn a visual claim into a machine check with a diff.\n",
    );
    out.trim_end().to_string()
}

fn list_cell(names: &[String]) -> String {
    if names.is_empty() {
        "-".to_string()
    } else {
        names
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn schema_properties(schema: &Value) -> Vec<String> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

/// `base` is the identity envelope every tool carries (`request_id` /
/// `session_id`); a cheat sheet says `session_id` defaults to this server's own
/// session, so repeating `base` in every row would be noise.
fn required_args(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|name| *name != "base")
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The one argument that makes an example worth pasting, for tools whose real
/// input is optional in the schema (`acos_launch` without `apk` boots nothing).
fn primary_args(tool_name: &str) -> &'static [&'static str] {
    match tool_name {
        "acos_launch" | "acos_verify" | "acos_inspect" | "acos_run" => &["apk"],
        "acos_run_dex" => &["main"],
        "acos_tap" => &["id"],
        "acos_text" => &["text"],
        "acos_key" => &["code"],
        "acos_observe" => &["tree"],
        "acos_swipe" => &["from", "to"],
        _ => &[],
    }
}

fn optional_args(schema: &Value) -> Vec<String> {
    let required = required_args(schema);
    // `base` is the identity envelope every session tool accepts; it is noise in
    // a cheat sheet, so it is left out of the optional column.
    schema_properties(schema)
        .into_iter()
        .filter(|name| !required.contains(name) && name != "base")
        .collect()
}

/// A paste-able example for `tool`, built from its required arguments plus the
/// one argument that actually selects what to act on.
fn example_call(tool: &ToolInfo) -> String {
    let mut names = required_args(&tool.input_schema);
    for primary in primary_args(&tool.tool_name) {
        if !names.iter().any(|name| name == primary) {
            names.push((*primary).to_string());
        }
    }
    let mut object = serde_json::Map::new();
    for name in names {
        let schema = tool
            .input_schema
            .get("properties")
            .and_then(|props| props.get(&name))
            .cloned()
            .unwrap_or(Value::Null);
        object.insert(name.clone(), sample_value(&name, &schema));
    }
    if object.is_empty() {
        format!("{} {{}}", tool.tool_name)
    } else {
        format!("{} {}", tool.tool_name, Value::Object(object))
    }
}

fn sample_value(name: &str, schema: &Value) -> Value {
    match name {
        "apk" => json!("fixtures/build/apps/counter.apk"),
        "scenario" => json!("scenarios/counter.yaml"),
        "artifacts" => json!("out/counter"),
        "path" => json!("fixtures/build/dexprogs/01_hello/classes.dex"),
        "main" => json!("Main"),
        "action" => json!("tap"),
        "scope" => json!("app"),
        "topic" => json!(MCP_TOPIC),
        "text" => json!("Increment"),
        "code" => json!("back"),
        "id" => json!("inc"),
        "label" => json!("my-run"),
        "tree" => json!(true),
        "from" => json!({"x": 540, "y": 1800}),
        "to" => json!({"x": 540, "y": 600}),
        _ => match schema.get("type").and_then(Value::as_str) {
            Some("integer") | Some("number") => json!(1),
            Some("boolean") => json!(true),
            Some("array") => json!([]),
            Some("object") => json!({}),
            _ => json!("<string>"),
        },
    }
}

/// What the tool returns, in one clause.
///
/// The table is explicit on purpose: a row that falls back to the generic text
/// is a *documentation* bug, and `crates/acos-mcp/tests/guide.rs` fails when an
/// advertised tool has no entry (so a new tool cannot ship undocumented here).
fn tool_returns(tool: &ToolInfo) -> &'static str {
    match tool.tool_name.as_str() {
        "acos_launch" => {
            "the launch ladder (`stages[]`), `reached_stage`, `package`/`activity`, \n`session_id`, `artifacts_dir`, `settle`, full `modules[]` and the frame identity; a non-ok rung carries the blocker"
        }
        "acos_observe_tree" => {
            "`nodes[]` (id, class, resource_id, text, content_desc, bounds, visible/\nenabled/focused/clickable/scrollable), `windows`, `truncated`, `max_nodes`, `tree_path`, `modules_summary`"
        }
        "acos_tap" => {
            "`handled` (the app window's own `finishInputEvent` answer), `accepted`, \n`state_changed`, `target` (resolved node + the hit-test at the injected point, \nwith the clickable node to tap instead), `acks`, `modules_summary` and `pixels_changed` when measured"
        }
        "acos_text" => {
            "`handled`, `accepted`, `state_changed`, `target` (the field the text went to), \n`text_path` (`input-connection` or `key-events`) and `modules_summary`"
        }
        "acos_key" => "`handled`, `accepted`, `state_changed`, `target`, `acks`, `modules_summary`",
        "acos_swipe" => {
            "`handled` (per-event `finishInputEvent` bit of a real DOWN/MOVE/UP stream), \n`accepted`, `state_changed`, `target`, the injected gesture seqs/acks, `modules_summary`, and `until` \n(`found`/`attempts`/`frame_seq`) when the call searched for a selector"
        }
        "acos_logs" => "`logs[]` (priority, tag, message, line, repeat_count), `total`/`count`, `filter`, `stage_reached`, `crash`, `modules_summary`",
        "acos_status" => "`launched`, `stage_reached`, `published_frames`, `package`/`activity`, `capabilities`, `modules_summary`",
        "acos_services" => "`attached[]` (`id`, `profile`) and full `modules[]` (`id`, `profile`, `attach`, `state`, `fidelity`, `reason`); attach/detach answer with the set they produced",
        "acos_service_ctl" => "`id`, `op`, `value` (the module's own answer) and `modules_summary` after the control (call `acos_services` for full module rows); a detached module is a structured refusal",
        "acos_capabilities" => "the implemented command list, the resolved `acos` binary/version, the server limits and — with `session_id` — that session's attached modules",
        "acos_net" => "`action`, the relay state (`up`, `pasta_version`, `host_ports`) and this \n                       session's own row (`networked`, `offline`, `delay_ms`, `loss_pct`); \n                       `hostport` also returns the relay's `host_ports` list",        "acos_doctor" => "`checks[]` with pass/fail and a concrete `fix` per failing check",
        "acos_help" => "`guide` (the requested section), `guide_topics`, `fidelity_rules`, `supported`/`unsupported`",
        "acos_inspect" => "package, version, min/target SDK, launcher, permissions, components, DEX/native/asset inventory, signature schemes",
        "acos_observe" => "the reference session's tree and/or text, plus frame metadata (frame_seq, cause_id, content_hash, meaningful)",
        "acos_run" => "`session_id`, package, effective isolation/display/fidelity and optionally the first frame (reference engine)",
        "acos_action" => "`accepted`, `state_changed`, the input-ack event seq and optionally a fresh frame",
        "acos_verify" => "`verdict` (pass/fail/unsupported per step), reached fidelity, `repro`, artifact paths and the wall time",
        "acos_reset" => "the relaunched session's ladder and a fresh frame identity (app data is empty)",
        "acos_destroy" => "`destroyed` and the session id; idempotent",
        "acos_run_dex" => "`exit_code`, `stdout`, `stderr` and any uncaught exception",
        // Not advertised in this build (hidden), so never rendered; a new tool
        // must add its own line here or the guide test fails.
        _ => "see the tool description (no cheat-sheet entry yet: add one to `help::tool_returns`)",
    }
}

/// True when [`tool_returns`] has a real entry (used by the guide test).
#[must_use]
pub fn has_returns_entry(tool_name: &str) -> bool {
    !tool_returns(&ToolInfo {
        tool_name: tool_name.to_string(),
        command: None,
        description: String::new(),
        input_schema: Value::Null,
        available: true,
        fidelity: Fidelity::NotApplicable,
        reason: None,
    })
    .starts_with("see the tool description")
}

/// One `<!-- topic:NAME -->` section of the guide.
fn section(topic: &str) -> Option<String> {
    let start = format!("<!-- topic:{topic} -->");
    let end = format!("<!-- /topic:{topic} -->");
    match (AGENT_GUIDE.find(&start), AGENT_GUIDE.find(&end)) {
        (Some(from), Some(to)) if to > from => {
            let raw = AGENT_GUIDE[from + start.len()..to].trim();
            Some(render_guide(raw).trim().to_string())
        }
        _ => None,
    }
}

/// Build the structured help payload.
pub fn build_help(
    caps: &Capabilities,
    info: &BackendInfo,
    compatibility_root: Option<&Path>,
) -> Value {
    build_help_topic(caps, info, compatibility_root, None)
}

/// `build_help` for one guide topic (`None` = the whole guide).
pub fn build_help_topic(
    caps: &Capabilities,
    info: &BackendInfo,
    compatibility_root: Option<&Path>,
    topic: Option<&str>,
) -> Value {
    let infos = tool_infos(caps);
    let supported: Vec<&str> = infos
        .iter()
        .filter(|t| t.available)
        .map(|t| t.tool_name.as_str())
        .collect();
    let unsupported: Vec<Value> = infos
        .iter()
        .filter(|t| !t.available)
        .map(|t| {
            json!({
                "tool": t.tool_name,
                "command": t.command.map(|c| c.as_str()),
                "reason": t.reason,
                "fidelity_when_available": t.fidelity.as_str(),
            })
        })
        .collect();

    let error_playbook = json!([
        {
            "category": "engine cannot do it (status=unsupported)",
            "codes": ["UNSUPPORTED_ANDROID_API", "UNSUPPORTED_JAVA_API", "UNSUPPORTED_BYTECODE",
                      "UNSUPPORTED_NATIVE_SYMBOL", "UNSUPPORTED_RELOCATION", "NEEDS_REAL_DEVICE",
                      "REQUIRES_GMS", "WRONG_ABI", "NO_DISPLAY", "NO_TEXT", "UNSUPPORTED_BACKEND",
                      "RENDER_UNSUPPORTED_OP"],
            "action": "Do not rewrite the app to hide the gap. Read details, try another fidelity/target, or report unsupported. Never a pass."
        },
        {
            "category": "the app is wrong (status=fail)",
            "codes": ["ASSERTION_FAILED", "ELEMENT_NOT_FOUND"],
            "action": "Read the assertion diff (expected/actual/path) and the linked frame/tree, then fix the app or the expectation and rerun."
        },
        {
            "category": "app crashed or hung",
            "codes": ["APP_CRASHED", "APP_NOT_RESPONDING", "JAVA_EXCEPTION"],
            "action": "Read the crash/ANR event and last actions; fix the app or the unsupported platform call. Never a pass."
        },
        {
            "category": "bad build artifact",
            "codes": ["APK_NOT_ZIP", "APK_NO_MANIFEST", "APK_IO", "APK_ENTRY",
                      "APK_SIGNING_MALFORMED", "AXML_MALFORMED", "ARSC_MALFORMED", "DEX_BAD_*",
                      "DEX_TRUNCATED", "DEX_VERIFY_FAILED", "DEX_LOAD_ERROR", "ELF_MALFORMED",
                      "NATIVE_DEP_NOT_FOUND", "NATIVE_INIT_FAILED", "NATIVE_SYMBOL_NOT_FOUND"],
            "action": "Rebuild/re-sign/re-package and retry inspect first; these are artifact problems, not app verdicts."
        },
        {
            "category": "session / resource problems",
            "codes": ["SESSION_NOT_FOUND", "SESSION_BUSY", "STALE_REF", "TIMEOUT", "RESOURCE_LIMIT",
                      "RAM_GUARD", "RAM_ABORT"],
            "action": "Create the session first, serialise mutations, re-observe after mutations, reduce concurrency or raise an explicit budget."
        },
        {
            "category": "evidence / fidelity problems",
            "codes": ["NO_FRESH_FRAME", "UNVERIFIED_FIDELITY", "ARTIFACTS_INVALID", "UNSAFE_PATH"],
            "action": "Wait for a new frame, use after_seq/after_cause, re-run at a qualifying fidelity, and write artifacts inside the artifacts directory."
        },
        {
            "category": "invalid input",
            "codes": ["INVALID_SCENARIO", "INVALID_ARGUMENT", "ISOLATION_UNAVAILABLE",
                      "ISOLATION_UNSATISFIED"],
            "action": "Fix every reported path/field; choose an isolation level the engine supports or report the missing guarantee."
        },
        {
            "category": "harness failure",
            "codes": ["VM_INTERNAL", "INTERNAL", "RENDER_*", "RENDER_STRICT"],
            "action": "An ACOS bug or strict-rendering rejection. Capture request/scenario/events; do not retry blindly or claim pass."
        }
    ]);

    let fidelity = Fidelity::StaticAnalysis;

    let compatibility = compatibility_root
        .and_then(read_compatibility_summary)
        .map(|summary| {
            json!({
                "source": "COMPATIBILITY.md",
                "summary": summary,
            })
        });

    let markdown = render_markdown(&supported, &unsupported);

    json!({
        "loop": "inspect -> verify -> read verdict.json -> fix -> rerun",
        "workflow": [
            "1. acos_inspect the APK (static only; never executes) to learn package, launcher, SDK, DEX/native inventory.",
            "2. acos_capability_report (once implemented) or acos_capabilities to check the engine can do what the task needs.",
            "3. acos_verify an APK + scenario into an artifacts directory; the verdict and step evidence are the product.",
            "4. Read verdict.json and steps/NN-*.json on a non-pass; never guess.",
            "5. Fix the app or the scenario, then rerun into a fresh directory. Reuse the same scenario so the comparison is meaningful.",
            "6. A pass is valid only when its evidence exists; `unsupported` is never a pass."
        ],
        "error_playbook": error_playbook,
        "fidelity_rules": [
            {"level": "static-analysis", "proves": "host APK/DEX parsing only; no runtime behaviour"},
            {"level": "jvm-reference", "proves": "host OpenJDK pure-Java semantics only; never Android/ART/framework/JNI/rendering"},
            {"level": "native-runtime", "proves": "ACOS executed it; not ART-validated unless validated_against says so"},
            {"level": "art-reference", "proves": "real ART on a reference image; authoritative for the stated API/ABI"},
            {"level": "physical-device", "proves": "a real device/OEM target"},
            {"level": "simulated", "proves": "the mock path only; not an Android success"},
            {"level": "n/a", "proves": "nothing about app behaviour: this is a meta reply (help/capabilities/doctor) about ACOS itself"}
        ],
        "supported": supported,
        "unsupported": unsupported,
        "backend": {
            "binary": info.binary.as_ref().map(|p| p.display().to_string()),
            "version": info.version,
            "probed": caps.probed,
            "probe_error": caps.probe_error,
        },
        "available_command_fidelity": infos.iter()
            .filter(|t| t.available)
            .filter_map(|t| t.command.map(|c| (t.tool_name.as_str(), command_fidelity(c).as_str())))
            .map(|(tool, fidelity)| json!({"tool": tool, "fidelity": fidelity}))
            .collect::<Vec<_>>(),
        "default_command_fidelity": fidelity.as_str(),
        "compatibility": compatibility,
        "markdown": markdown,
        // The agent guide (embedded from `docs/AGENT_GUIDE.md`): the full text,
        // the requested topic when one was asked for, and the topic list.
        "guide": guide_with(topic, &infos),
        "guide_topic": match topic {
            Some(topic) if is_topic(topic) => Some(topic),
            _ => None,
        },
        "guide_unknown_topic": topic.is_some_and(|t| !is_topic(t)),
        "guide_topics": topic_list(),
        "guide_all_topic": ALL_TOPIC,
        "guide_source": "docs/AGENT_GUIDE.md",
    })
}

fn render_markdown(supported: &[&str], unsupported: &[Value]) -> String {
    let mut out = String::new();
    out.push_str("# ACOS via MCP\n\n");
    out.push_str("Loop: **inspect -> verify -> read verdict -> fix -> rerun**.\n\n");
    out.push_str("## Currently available tools\n\n");
    if supported.is_empty() {
        out.push_str("- (none — the `acos` backend was not found)\n");
    } else {
        for tool in supported {
            out.push_str(&format!("- `{tool}`\n"));
        }
    }
    out.push_str("\n## Not implemented yet (returns `unsupported`, never a pass)\n\n");
    for item in unsupported {
        let tool = item.get("tool").and_then(Value::as_str).unwrap_or("?");
        let reason = item
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("not implemented");
        out.push_str(&format!("- `{tool}` — {reason}\n"));
    }
    out.push_str("\nCall `acos_help` with a topic for the full playbook, or `acos_capabilities` to re-probe.\n");
    out
}

/// Read the headline lines of `COMPATIBILITY.md` without slurping the whole file.
fn read_compatibility_summary(root: &Path) -> Option<String> {
    let path = root.join("COMPATIBILITY.md");
    let text = std::fs::read_to_string(&path).ok()?;
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("> **As of") {
            lines.push(trimmed.trim_start_matches('>').trim().to_string());
        }
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lists_supported_and_unsupported() {
        let mut caps = Capabilities::unprobed();
        caps.supported.insert(acos_protocol::CommandName::Inspect);
        let info = BackendInfo {
            binary: None,
            version: None,
        };
        let help = build_help(&caps, &info, None);
        assert!(help["supported"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "acos_inspect"));
        assert!(help["markdown"].as_str().unwrap().contains("acos_inspect"));
        assert!(help["loop"].as_str().unwrap().contains("inspect"));
    }

    #[test]
    fn compatibility_summary_is_read_when_present() {
        let dir = std::env::temp_dir().join(format!("acos-help-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("COMPATIBILITY.md"),
            "> **As of 2026-11-10: static DEX/APK analysis is verified; nothing executes.**\n",
        )
        .unwrap();
        let caps = Capabilities::unprobed();
        let info = BackendInfo {
            binary: None,
            version: None,
        };
        let help = build_help(&caps, &info, Some(&dir));
        assert!(help["compatibility"]["summary"]
            .as_str()
            .unwrap()
            .contains("static DEX/APK analysis"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
