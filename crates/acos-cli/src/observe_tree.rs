//! Structured UI tree extraction (`observe --tree`) and the offline tree
//! parsers shared with `acos parity`.
//!
//! The live tree comes from [`acos_runtime::view_tree`], which walks the real
//! framework `View` hierarchy inside the VM. This module only adapts it to the
//! JSON contract and parses the two captures a parity run compares against:
//!
//! * `uiautomator dump` XML (`<node text=... resource-id=... bounds=...>`);
//! * the agent-device accessibility snapshot JSON (`data.nodes[]`, a flat list
//!   with an integer `depth` and a `rect`);
//! * an ACOS `observe --tree` tree JSON (our own output format).
//!
//! Everything here is pure so it can be tested against the recorded captures
//! under `fixtures/reference-device/` without a phone or a VM.

use acos_protocol::ui::Bounds;
use acos_runtime::view_tree::{ViewNode, ViewTree};
use serde_json::{json, Value};

/// Render one live [`ViewTree`] as the `observe --tree` `result.tree` object.
#[must_use]
pub fn tree_to_json(tree: &ViewTree) -> Value {
    json!({
        "windows": tree.windows,
        "truncated": tree.truncated,
        "max_nodes": tree.max_nodes,
        "nodes": tree.nodes.iter().map(node_to_json).collect::<Vec<_>>(),
    })
}

fn node_to_json(node: &ViewNode) -> Value {
    json!({
        "id": node.id,
        "class": node.class,
        "resource_id": node.resource_id,
        "text": node.text,
        "content_desc": node.content_desc,
        "bounds": node.bounds,
        "visible": node.visible,
        "enabled": node.enabled,
        "focused": node.focused,
        "clickable": node.clickable,
        "scrollable": node.scrollable,
        "checked": node.checked,
        "package": node.package,
        "children": node.children.iter().map(node_to_json).collect::<Vec<_>>(),
    })
}

/// The canonical flat text rendering (one line per node with text/desc).
#[must_use]
pub fn tree_text(tree: &ViewTree) -> String {
    let mut out = Vec::new();
    fn walk(nodes: &[ViewNode], out: &mut Vec<String>) {
        for node in nodes {
            if !node.text.is_empty() || !node.content_desc.is_empty() {
                out.push(format!(
                    "{} | {} | {} | [{},{},{},{}]",
                    node.class,
                    node.resource_id,
                    node.text,
                    node.bounds[0],
                    node.bounds[1],
                    node.bounds[2],
                    node.bounds[3]
                ));
            }
            walk(&node.children, out);
        }
    }
    walk(&tree.nodes, &mut out);
    out.join("\n")
}

/// Every node of a forest in stable pre-order.
#[must_use]
pub fn flatten(nodes: &[ViewNode]) -> Vec<&ViewNode> {
    let mut out = Vec::new();
    fn walk<'a>(nodes: &'a [ViewNode], out: &mut Vec<&'a ViewNode>) {
        for node in nodes {
            out.push(node);
            walk(&node.children, out);
        }
    }
    walk(nodes, &mut out);
    out
}

// ---------------------------------------------------------------------------
// uiautomator XML
// ---------------------------------------------------------------------------

/// Parse a `uiautomator dump` hierarchy into a forest of [`ViewNode`]s.
///
/// This is a deliberately small, dependency-free parser: it understands the
/// attribute order used by uiautomator, XML entity decoding and both the
/// `<node .../>` and `<node ...> ... </node>` forms.
pub fn parse_uiautomator_xml(xml: &str) -> Result<Vec<ViewNode>, String> {
    let mut roots: Vec<ViewNode> = Vec::new();
    // Stack of (node, path-from-root) is awkward in Rust; keep a stack of
    // partially-built nodes and a parallel stack of completed child lists.
    let mut stack: Vec<ViewNode> = Vec::new();
    let mut counter = 0usize;

    let bytes = xml.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Find the end of this tag, honoring quoted attribute values.
        let mut j = i + 1;
        let mut quote: Option<u8> = None;
        while j < bytes.len() {
            let b = bytes[j];
            match quote {
                Some(q) => {
                    if b == q {
                        quote = None;
                    }
                }
                None => {
                    if b == b'"' || b == b'\'' {
                        quote = Some(b);
                    } else if b == b'>' {
                        break;
                    }
                }
            }
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        let raw = &xml[i + 1..j];
        let (name, body, self_closing) = split_tag(raw);
        match name {
            "node" => {
                counter += 1;
                let node = node_from_attrs(body, counter)?;
                if self_closing {
                    attach(&mut stack, &mut roots, node);
                } else {
                    stack.push(node);
                }
            }
            "/node" => {
                if let Some(node) = stack.pop() {
                    attach(&mut stack, &mut roots, node);
                }
            }
            _ => {}
        }
        i = j + 1;
    }
    Ok(roots)
}

fn split_tag(raw: &str) -> (&str, &str, bool) {
    let trimmed = raw.trim();
    let self_closing = trimmed.ends_with('/');
    let trimmed = if self_closing {
        trimmed[..trimmed.len() - 1].trim_end()
    } else {
        trimmed
    };
    match trimmed.find(char::is_whitespace) {
        Some(pos) => (&trimmed[..pos], trimmed[pos..].trim(), self_closing),
        None => (trimmed, "", self_closing),
    }
}

fn attach(stack: &mut [ViewNode], roots: &mut Vec<ViewNode>, node: ViewNode) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(node),
        None => roots.push(node),
    }
}

fn node_from_attrs(body: &str, counter: usize) -> Result<ViewNode, String> {
    let attrs = parse_attrs(body);
    let get = |key: &str| attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let bounds = get("bounds")
        .and_then(|s| s.parse::<Bounds>().ok())
        .map(|b| b.0)
        .unwrap_or([0, 0, 0, 0]);
    let visible = get("visible-to-user")
        .map(|v| v == "true")
        .unwrap_or_else(|| get("displayed").map(|v| v == "true").unwrap_or(true));
    Ok(ViewNode {
        id: format!("n{counter}"),
        class: get("class").unwrap_or_default().to_string(),
        resource_id: get("resource-id").unwrap_or_default().to_string(),
        text: get("text").unwrap_or_default().to_string(),
        content_desc: get("content-desc").unwrap_or_default().to_string(),
        bounds,
        visible,
        enabled: get("enabled").map(|v| v == "true").unwrap_or(true),
        focused: get("focused").map(|v| v == "true").unwrap_or(false),
        clickable: get("clickable").map(|v| v == "true").unwrap_or(false),
        scrollable: get("scrollable").map(|v| v == "true").unwrap_or(false),
        checked: get("checked").map(|v| v == "true").unwrap_or(false),
        package: get("package").unwrap_or_default().to_string(),
        children: Vec::new(),
    })
}

fn parse_attrs(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let rest_trimmed = rest.trim_start();
        if rest_trimmed.is_empty() {
            break;
        }
        let Some(eq) = rest_trimmed.find('=') else {
            break;
        };
        let key = rest_trimmed[..eq].trim();
        let after_eq = &rest_trimmed[eq + 1..];
        if after_eq.is_empty() {
            break;
        }
        let (value, consumed) = parse_quoted(after_eq);
        out.push((key.to_string(), decode_xml(&value)));
        rest = &after_eq[consumed..];
    }
    out
}

fn parse_quoted(s: &str) -> (String, usize) {
    let mut chars = s.char_indices();
    let Some((_, quote)) = chars.next() else {
        return (String::new(), 0);
    };
    if quote != '"' && quote != '\'' {
        return (String::new(), 0);
    }
    let start = quote.len_utf8();
    let mut end = s.len();
    for (idx, ch) in s[start..].char_indices() {
        if ch == quote {
            end = start + idx;
            break;
        }
    }
    let value = s[start..end].to_string();
    let consumed = if end < s.len() { end + quote.len_utf8() } else { end };
    (value, consumed)
}

fn decode_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find('&') {
        out.push_str(&rest[..idx]);
        let tail = &rest[idx..];
        let Some(semi) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..semi];
        let replacement = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix('#')
                .and_then(|n| {
                    if let Some(hex) = n.strip_prefix('x').or_else(|| n.strip_prefix('X')) {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        n.parse::<u32>().ok()
                    }
                })
                .and_then(char::from_u32),
        };
        match replacement {
            Some(ch) => out.push(ch),
            None => {
                out.push_str(&tail[..=semi]);
            }
        }
        rest = &tail[semi + 1..];
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------------------
// agent-device accessibility snapshot
// ---------------------------------------------------------------------------

/// Parse an agent-device `snapshot` JSON (`data.nodes`, a flat list with `depth`
/// and `rect`) into a forest.
pub fn parse_agent_device_snapshot(value: &Value) -> Result<Vec<ViewNode>, String> {
    let nodes = value
        .get("data")
        .and_then(|d| d.get("nodes"))
        .or_else(|| value.get("nodes"))
        .and_then(Value::as_array)
        .ok_or_else(|| "snapshot JSON has no data.nodes array".to_string())?;

    let mut roots: Vec<ViewNode> = Vec::new();
    // (depth, node index in the current parent's children); we rebuild by
    // depth using a stack of indices into a flattened arena.
    let mut stack: Vec<(usize, ViewNode)> = Vec::new();
    for (i, raw) in nodes.iter().enumerate() {
        let depth = raw.get("depth").and_then(Value::as_u64).unwrap_or(0) as usize;
        let node = view_node_from_device(raw, i + 1);
        while let Some((d, _)) = stack.last() {
            if *d >= depth {
                let (_, finished) = stack.pop().unwrap();
                push_finished(&mut stack, &mut roots, finished);
            } else {
                break;
            }
        }
        stack.push((depth, node));
    }
    while let Some((_, finished)) = stack.pop() {
        push_finished(&mut stack, &mut roots, finished);
    }
    Ok(roots)
}

fn push_finished(stack: &mut [(usize, ViewNode)], roots: &mut Vec<ViewNode>, node: ViewNode) {
    // `stack` is always used with its last element as the current parent; the
    // slice split keeps the borrow simple.
    if let Some((_, parent)) = stack.last_mut() {
        parent.children.push(node);
    } else {
        roots.push(node);
    }
}

fn view_node_from_device(raw: &Value, counter: usize) -> ViewNode {
    let rect = raw.get("rect");
    let x = rect.and_then(|r| r.get("x")).and_then(Value::as_i64).unwrap_or(0) as i32;
    let y = rect.and_then(|r| r.get("y")).and_then(Value::as_i64).unwrap_or(0) as i32;
    let w = rect
        .and_then(|r| r.get("width"))
        .and_then(Value::as_i64)
        .unwrap_or(0) as i32;
    let h = rect
        .and_then(|r| r.get("height"))
        .and_then(Value::as_i64)
        .unwrap_or(0) as i32;
    ViewNode {
        id: format!("n{counter}"),
        class: raw
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        resource_id: raw
            .get("identifier")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        text: raw
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        content_desc: raw
            .get("contentDescription")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        bounds: [x, y, x + w, y + h],
        visible: raw
            .get("visibleToUser")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        enabled: raw.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        focused: raw.get("focused").and_then(Value::as_bool).unwrap_or(false),
        clickable: raw
            .get("clickable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        scrollable: raw
            .get("scrollable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        checked: raw
            .get("checked")
            .or_else(|| raw.get("selected"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        package: raw
            .get("bundleId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        children: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// ACOS observe --tree JSON
// ---------------------------------------------------------------------------

/// Parse an `observe --tree` JSON envelope (or a bare `result.tree`) into a
/// forest.
pub fn parse_acos_tree_json(value: &Value) -> Result<Vec<ViewNode>, String> {
    let tree = value
        .get("result")
        .and_then(|r| r.get("tree"))
        .or_else(|| value.get("tree"))
        .unwrap_or(value);
    let nodes = tree
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "ACOS tree JSON has no nodes array".to_string())?;
    let mut counter = 0usize;
    let parsed: Vec<ViewNode> = nodes
        .iter()
        .map(|n| acos_node_from_json(n, &mut counter))
        .collect();
    Ok(parsed)
}

fn acos_node_from_json(raw: &Value, counter: &mut usize) -> ViewNode {
    *counter += 1;
    let bounds = raw
        .get("bounds")
        .and_then(Value::as_array)
        .map(|a| {
            let mut out = [0i32; 4];
            for (i, v) in a.iter().take(4).enumerate() {
                out[i] = v.as_i64().unwrap_or(0) as i32;
            }
            out
        })
        .unwrap_or([0, 0, 0, 0]);
    let children = raw
        .get("children")
        .and_then(Value::as_array)
        .map(|cs| {
            cs.iter()
                .map(|c| acos_node_from_json(c, counter))
                .collect()
        })
        .unwrap_or_default();
    ViewNode {
        id: raw
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("n{}", *counter)),
        class: string_field(raw, "class"),
        resource_id: string_field(raw, "resource_id"),
        text: string_field(raw, "text"),
        content_desc: string_field(raw, "content_desc"),
        bounds,
        visible: raw.get("visible").and_then(Value::as_bool).unwrap_or(true),
        enabled: raw.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        focused: raw.get("focused").and_then(Value::as_bool).unwrap_or(false),
        clickable: raw
            .get("clickable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        scrollable: raw
            .get("scrollable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        checked: raw.get("checked").and_then(Value::as_bool).unwrap_or(false),
        package: string_field(raw, "package"),
        children,
    }
}

fn string_field(raw: &Value, key: &str) -> String {
    raw.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn uiautomator_xml_parses_nested_nodes_and_entities() {
        let xml = r#"<?xml version='1.0'?><hierarchy rotation="0"><node index="0" text="a &amp; b" resource-id="pkg:id/root" class="android.widget.FrameLayout" package="pkg" content-desc="" checkable="false" checked="false" clickable="false" enabled="true" focusable="false" focused="false" scrollable="false" bounds="[0,0][1080,2400]"><node index="0" text="Hello" resource-id="pkg:id/title" class="android.widget.TextView" package="pkg" content-desc="" checkable="false" checked="false" clickable="true" enabled="true" focusable="true" focused="false" scrollable="false" bounds="[42,317][231,368]"/></node></hierarchy>"#;
        let roots = parse_uiautomator_xml(xml).unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].text, "a & b");
        assert_eq!(roots[0].children.len(), 1);
        let child = &roots[0].children[0];
        assert_eq!(child.text, "Hello");
        assert_eq!(child.bounds, [42, 317, 231, 368]);
        assert!(child.clickable);
        assert!(!child.focused);

        let flat = flatten(&roots);
        assert_eq!(flat.len(), 2);
    }

    #[test]
    fn acos_tree_json_round_trips() {
        let tree = ViewTree {
            nodes: vec![ViewNode {
                id: "n1".into(),
                class: "android.widget.TextView".into(),
                resource_id: "pkg:id/x".into(),
                text: "hi".into(),
                content_desc: String::new(),
                bounds: [1, 2, 3, 4],
                visible: true,
                enabled: true,
                focused: false,
                clickable: false,
                scrollable: false,
                checked: false,
                package: "pkg".into(),
                children: vec![],
            }],
            windows: 1,
            truncated: false,
            max_nodes: 2000,
        };
        let json = tree_to_json(&tree);
        let parsed = parse_acos_tree_json(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].text, "hi");
        assert_eq!(parsed[0].bounds, [1, 2, 3, 4]);
        assert_eq!(tree_text(&tree), "android.widget.TextView | pkg:id/x | hi | [1,2,3,4]");
    }

    #[test]
    fn recorded_device_snapshot_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/reference-device/counter/snapshot-00-launch.json");
        let text = std::fs::read_to_string(&path).expect("recorded capture");
        let value: Value = serde_json::from_str(&text).unwrap();
        let roots = parse_agent_device_snapshot(&value).unwrap();
        let flat = flatten(&roots);
        assert!(flat.iter().any(|n| n.resource_id == "dev.acos.fixtures.counter:id/count"));
        // Depth is preserved: the status-bar clock is a root, the app content
        // is nested.
        assert!(flat.len() > 5);
    }
}
