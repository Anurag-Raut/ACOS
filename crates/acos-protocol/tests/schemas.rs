//! The checked-in JSON Schemas must match the Rust types.

use std::path::Path;

use acos_protocol::schema::schema_documents;

fn text(value: &serde_json::Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("schema serializes");
    text.push('\n');
    text
}

#[test]
fn schemas_are_up_to_date() {
    let schema_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema");
    let update = std::env::var_os("UPDATE_SCHEMA").is_some();
    if update {
        std::fs::create_dir_all(&schema_dir).expect("create schema dir");
    }

    let mut stale = Vec::new();
    let documents = schema_documents();
    assert!(
        documents.len() >= 20,
        "expected a schema per command family"
    );

    for (name, value) in documents {
        let path = schema_dir.join(name);
        let expected = text(&value);
        if update {
            std::fs::write(&path, &expected).expect("write schema");
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(actual) if actual == expected => {}
            Ok(_) => stale.push(format!("{name} (content differs)")),
            Err(_) => stale.push(format!("{name} (missing)")),
        }
    }

    assert!(
        stale.is_empty(),
        "checked-in JSON Schemas are stale: {stale:?}. Refresh with:\n    UPDATE_SCHEMA=1 cargo test -p acos-protocol"
    );
}

#[test]
fn every_command_has_a_request_schema() {
    let names: Vec<&str> = schema_documents()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    for expected in [
        "envelope.json",
        "protocol-error.json",
        "commands.json",
        "inspect-request.json",
        "run-dex-request.json",
        "run-request.json",
        "observe-request.json",
        "capture-request.json",
        "action-request.json",
        "assert-request.json",
        "events-request.json",
        "reset-request.json",
        "profile-request.json",
        "destroy-request.json",
        "verify-request.json",
        "capability-report-request.json",
        "scenario.json",
        "ui-tree.json",
        "event.json",
        "verdict.json",
    ] {
        assert!(
            names.contains(&expected),
            "missing schema document {expected}"
        );
    }
}
