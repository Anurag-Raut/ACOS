//! ARGS, the other direction: every argument an agent is **told** to send must
//! be an argument the schema accepts.
//!
//! The closed-schema work (feedback 2026-11-15) stopped a documented-but-ignored
//! argument from looking like a success. It also created the opposite failure:
//! schemas were closed around the *typed protocol structs* while the session
//! backends kept reading more (`session_id` at the top level, `acos_launch
//! {flutter: true}`, `acos_observe {png: true}`), so documented, working calls
//! were rejected with `INVALID_ARGUMENT` — the docs and the server disagreed and
//! neither was obviously wrong.
//!
//! This test is the alarm for that class: it harvests every tool call written
//! down in `docs/*.md`, `tools/**/*mcp*.py` and `crates/acos-mcp/tests/*.rs`,
//! extracts the top-level argument names of each call, and runs them through the
//! very check the dispatcher runs (`acos_mcp::args::check`) against the very
//! schema `tools/list` publishes.
//!
//! It is deliberately a *scanner*, not a list of hand-copied examples: a new
//! documented argument that the schema does not know fails here the moment it is
//! written, which is the only moment anyone can fix it cheaply.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use acos_mcp::descriptors::known_tool_names;
use common::repo_root;
use serde_json::{json, Map, Value};

/// One tool call found in a source file: the tool, the top-level argument names
/// of its argument object, and where it was written.
#[derive(Debug, Clone)]
struct Found {
    tool: String,
    arguments: Vec<String>,
    file: PathBuf,
    line: usize,
}

/// Every `acos_*` tool name this build knows.
fn tools() -> BTreeSet<String> {
    known_tool_names().into_iter().collect()
}

/// Files whose examples are part of the agent contract.
fn sources(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let docs = root.join("docs");
    if let Ok(entries) = std::fs::read_dir(&docs) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            // A `PLAN_*.md` is a roadmap: it shows API that does not exist yet
            // (`acos_launch {services: ...}`), so it is not a promise the schema
            // can keep. Every other doc is the agent contract.
            if name.starts_with("PLAN_") {
                continue;
            }
            if path.extension().is_some_and(|extension| extension == "md") {
                files.push(path);
            }
        }
    }
    collect_matching(&root.join("tools"), &mut files);
    let tests = root.join("crates/acos-mcp/tests");
    if let Ok(entries) = std::fs::read_dir(&tests) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            // `args.rs` is the test that *sends* unknown arguments on purpose
            // (`{"screenshots": true}`, `{"verbose": true}`) to prove they are
            // rejected; this file quotes the same text. Neither is a documented
            // call an agent is told to make.
            if name == "args.rs" || name == "documentation_args.rs" {
                continue;
            }
            if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Recursively collect `*mcp*.py` under `dir` (the MCP smoke/probe scripts).
fn collect_matching(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_matching(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "py")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("mcp"))
        {
            files.push(path);
        }
    }
}

/// Is `text[at..]` the start of a tool name token (`acos_observe_tree`)?
///
/// A name that continues into another identifier character is a longer name
/// (`acos_observe_tree_x`), not this tool.
fn tool_at(text: &str, at: usize, tool: &str) -> bool {
    let rest = &text[at..];
    let Some(rest) = rest.strip_prefix(tool) else {
        return false;
    };
    // The name may be quoted (`"acos_launch"`) — the caller handles the prefix.
    !rest
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Walk from just after a tool name to the `{` of its argument object.
///
/// The written forms are `acos_x {...}` (docs), `"acos_x", {...}` /
/// `call_tool("acos_x", {...})` (tests, python) and
/// `{"name": "acos_x", "arguments": {...}}` (docs/MCP.md). Only those links are
/// skipped — whitespace, a comma, parentheses, `&`, `!`, `json!` and the
/// literal key `"arguments"`. Anything else means the text after the name is not
/// that call's arguments (prose, a longer identifier, a property access), so the
/// occurrence is ignored rather than guessed at.
fn advance_to_object(text: &str, mut cursor: usize) -> Option<usize> {
    loop {
        let bytes = text.as_bytes();
        while cursor < bytes.len()
            && matches!(
                bytes[cursor],
                b' ' | b'\t' | b'\r' | b'\n' | b',' | b'(' | b'&' | b'!'
            )
        {
            cursor += 1;
        }
        let rest = &text[cursor..];
        if rest.starts_with("json!") {
            cursor += "json!".len();
            continue;
        }
        if rest.starts_with('"') {
            // The `"arguments":` key of the JSON call form.
            if let Some(after) = rest.strip_prefix("\"arguments\"") {
                let trimmed = after.trim_start();
                if trimmed.starts_with(':') {
                    cursor += "\"arguments\"".len() + (after.len() - trimmed.len()) + 1;
                    continue;
                }
            }
            // The closing quote of a quoted tool name: a call site continues
            // with a comma (`call_tool("acos_x", {...})`,
            // `"name": "acos_x", "arguments": {...}`). `tool == "acos_x" {`
            // (an `if` block) is not a call and stops here.
            let after_name = rest[1..].trim_start();
            if after_name.starts_with(',') {
                cursor += 1 + (rest.len() - 1 - after_name.len()) + 1;
                continue;
            }
            return None;
        }
        if bytes.get(cursor) == Some(&b'{') {
            return Some(cursor);
        }
        return None;
    }
}

/// The top-level keys of the flow-style object starting at `start`.
///
/// `text[start]` must be `{`. The scanner tracks string/escape/depth, so
/// `"base": {"session_id": "x"}` contributes `base` only, and both spellings of
/// a key (`"screenshot": true` in JSON, `screenshot: true` in a YAML flow map)
/// are read the same way.
fn object_keys(text: &str, start: usize) -> Option<(Vec<String>, usize)> {
    let bytes = text.as_bytes();
    if *bytes.get(start)? != b'{' {
        return None;
    }
    let mut keys = Vec::new();
    let mut depth = 0usize;
    let mut string = false;
    let mut escape = false;
    let mut index = start;
    let mut expect_key = true;
    while index < bytes.len() {
        let byte = bytes[index];
        if string {
            if escape {
                escape = false;
            } else if byte == b'\\' {
                escape = true;
            } else if byte == b'"' {
                string = false;
            }
            index += 1;
            continue;
        }
        match byte {
            b'[' | b'{' => {
                depth += 1;
                if depth > 1 {
                    expect_key = false;
                }
            }
            b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((keys, index + 1));
                }
            }
            b'"' => {
                string = true;
                if depth == 1 && expect_key {
                    let after = &text[index + 1..];
                    if let Some(end) = after.find('"') {
                        let key = &after[..end];
                        // `"topic\": ...` is an escaped quote inside a Rust
                        // string literal, not a key: a backslash means this
                        // quotation is not the flow text it looks like.
                        if !key.contains('\\') && after[end + 1..].trim_start().starts_with(':') {
                            keys.push(key.to_string());
                            expect_key = false;
                        }
                    }
                }
            }
            b',' => {
                if depth == 1 {
                    expect_key = true;
                }
            }
            byte if depth == 1 && expect_key && (byte.is_ascii_alphabetic() || byte == b'_') => {
                // YAML flow-map spelling: `png:true`.
                let rest = &text[index..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                    .unwrap_or(rest.len());
                let name = &rest[..end];
                if rest[end..].trim_start().starts_with(':') {
                    keys.push(name.to_string());
                    expect_key = false;
                }
                index += end;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())]
        .bytes()
        .filter(|b| *b == b'\n')
        .count()
        + 1
}

/// Every tool call written in `text` (see the module docs for the forms).
fn scan(path: &Path, text: &str, tools: &BTreeSet<String>) -> Vec<Found> {
    let mut found = Vec::new();
    for tool in tools {
        let mut search = 0usize;
        while let Some(relative) = text[search..].find(tool.as_str()) {
            let at = search + relative;
            search = at + tool.len();
            if !tool_at(text, at, tool) {
                continue;
            }
            let Some(cursor) = advance_to_object(text, at + tool.len()) else {
                continue;
            };
            if let Some((arguments, _)) = object_keys(text, cursor) {
                if arguments.is_empty() {
                    continue;
                }
                found.push(Found {
                    tool: tool.clone(),
                    arguments,
                    file: path.to_path_buf(),
                    line: line_of(text, at),
                });
            }
        }
    }
    found
}

#[test]
fn every_documented_argument_is_accepted_by_its_tool_schema() {
    let root = repo_root();
    let tools = tools();
    let mut calls = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for path in sources(&root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let relative = path.strip_prefix(&root).unwrap_or(&path).display();
        for found in scan(&path, &text, &tools) {
            calls += 1;
            let Some(schema) = acos_mcp::descriptors::tool_schema(&found.tool) else {
                failures.push(format!(
                    "{relative}:{}: `{}` has no published schema",
                    found.line, found.tool
                ));
                continue;
            };
            let mut object = Map::new();
            for name in &found.arguments {
                object.insert(name.clone(), Value::Null);
            }
            if let Some(envelope) = acos_mcp::args::check(&found.tool, &json!(object), &schema) {
                let error = envelope.error();
                failures.push(format!(
                    "{relative}:{}: `{}` {} is documented but its schema rejects it: {}",
                    found.line,
                    found.tool,
                    found.arguments.join(", "),
                    error
                        .map(|error| error.message.clone())
                        .unwrap_or_else(|| "rejected".to_string())
                ));
            }
        }
    }
    assert!(
        calls > 40,
        "the scanner found only {calls} documented calls; it is not looking at the docs"
    );
    assert!(
        failures.is_empty(),
        "documented arguments must be accepted by the schema the server publishes:\n{}",
        failures.join("\n")
    );
}

#[test]
fn the_scanner_reads_both_json_and_flow_style_arguments() {
    let tools: BTreeSet<String> = ["acos_observe".to_string()].into_iter().collect();
    let json_form =
        r#"{"name": "acos_observe", "arguments": {"png": true, "base": {"session_id": "s"}}}"#;
    let found = scan(Path::new("doc.md"), json_form, &tools);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].arguments, vec!["png", "base"], "{found:?}");

    let flow_form = "`acos_observe {png:true, screenshot:path}`";
    let found = scan(Path::new("doc.md"), flow_form, &tools);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].arguments, vec!["png", "screenshot"], "{found:?}");

    // A call site in a test, with the json! macro and a multi-line object.
    let rust_form = "backend.call(\n  \"acos_observe\",\n  &json!({\n    \"tree\": false,\n    \"png\": true,\n  }),\n);";
    let found = scan(Path::new("test.rs"), rust_form, &tools);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].arguments, vec!["tree", "png"], "{found:?}");

    // Prose that names the tool without a call is not a call.
    let prose = "`acos_observe` and `acos_observe_tree` both take `screenshot`:";
    assert!(scan(Path::new("doc.md"), prose, &tools).is_empty());
}

#[test]
fn the_three_arguments_this_test_was_written_for_are_in_their_schemas() {
    // The review's exact findings, kept as a direct assertion next to the
    // scanner: each of these is documented and honoured by the backend, and each
    // was rejected by the closed schema before this fix.
    for (tool, argument) in [
        ("acos_observe_tree", "session_id"),
        ("acos_launch", "session_id"),
        ("acos_launch", "flutter"),
        ("acos_launch", "rn"),
        ("acos_observe", "session_id"),
        ("acos_observe", "png"),
        ("acos_observe", "screenshot"),
    ] {
        let schema = acos_mcp::descriptors::tool_schema(tool).expect(tool);
        assert!(
            schema["properties"].get(argument).is_some(),
            "`{tool}` must publish `{argument}`: {}",
            schema["properties"]
        );
        assert!(acos_mcp::args::check(tool, &json!({argument: null}), &schema).is_none());
        // …and it is described, not just listed.
        assert!(
            schema["properties"][argument]["description"]
                .as_str()
                .is_some_and(|description| !description.trim().is_empty()),
            "`{tool}.{argument}` must carry a description: {}",
            schema["properties"][argument]
        );
    }
    // `acos_doctor` is a host preflight: it has no session to address, so it
    // must not publish a `session_id` it would ignore.
    let doctor = acos_mcp::descriptors::tool_schema("acos_doctor").expect("acos_doctor");
    assert!(doctor["properties"].get("session_id").is_none(), "{doctor}");
}

/// The `engine` argument's published description states both defaults: `art`,
/// and `rust-vm` when `reference` is set.
///
/// Before this, the schema said only that `rust-vm` was the default while the
/// code ran `art` — one of the two documented front doors was lying. The
/// `reference` rule is part of the contract, so it is in the one place a client
/// reads.
#[test]
fn the_engine_argument_documents_the_reference_default() {
    let schema = acos_mcp::descriptors::tool_schema("acos_verify").expect("acos_verify");
    let engine = &schema["properties"]["engine"];
    assert_eq!(
        engine["default"],
        json!(acos_protocol::commands::DEFAULT_VERIFY_ENGINE_NAME),
        "{engine}"
    );
    let description = engine["description"].as_str().unwrap_or_default();
    assert!(description.contains("rust-vm"), "{description}");
    assert!(
        description.contains("reference"),
        "the description must state that `reference` selects `rust-vm` when `engine` is \
         omitted: {description}"
    );
}
