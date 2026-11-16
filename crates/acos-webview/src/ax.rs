//! The page's accessibility tree, exported for merging into the ACOS UI tree.
//!
//! Two DevTools calls do all the work:
//!
//! * `Accessibility.getFullAXTree` gives roles, names, values and the state
//!   flags (`focused`, `editable`, `checked`, `disabled`);
//! * `DOMSnapshot.captureSnapshot` gives **one** layout snapshot keyed by
//!   `backendNodeId`, so bounds cost one round trip for the whole page instead
//!   of `DOM.getBoxModel` per node (an 800-node page would otherwise be 800
//!   round trips on the critical path of every `ax`).
//!
//! Bounds are converted from document CSS pixels to **display coordinates**
//! (device pixels, view origin at 0,0) with `Page.getLayoutMetrics`'s visual
//! viewport scroll offset and the view's density — the same space `input.tap`
//! takes, so a guest merge needs no transform.

use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::error::Result;
use crate::view::{round1, View};

/// One exported node.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AxNode {
    pub node_id: i64,
    pub parent_id: Option<i64>,
    pub role: String,
    pub name: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    /// `[x, y, w, h]` in device pixels; zeros when the node has no layout box.
    pub bounds: [f64; 4],
    pub has_bounds: bool,
    pub clickable: bool,
    pub editable: bool,
    pub focused: bool,
    pub checked: Option<String>,
    pub disabled: bool,
    pub backend_dom_node_id: Option<i64>,
}

/// Roles that are only interesting when they carry a name, or are pure layout
/// noise for an agent.
const NOISY_ROLES: &[&str] = &["InlineTextBox", "none", "generic", "LineBreak", "paragraph"];

const CLICKABLE_ROLES: &[&str] = &[
    "button",
    "link",
    "checkbox",
    "radio",
    "switch",
    "tab",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "listbox",
    "textbox",
    "searchbox",
    "combobox",
    "slider",
    "spinbutton",
];

/// Export the view's AX tree.
pub fn export(view: &View, max_nodes: Option<usize>, include_ignored: bool) -> Result<Value> {
    let timeout = Duration::from_secs(30);
    let tree = view.call("Accessibility.getFullAXTree", json!({}), timeout)?;
    let bounds = layout_bounds(view).unwrap_or_default();
    let (scroll_x, scroll_y) = scroll_offset(view).unwrap_or((0.0, 0.0));
    let vp = view.viewport();
    let limit = max_nodes.unwrap_or(5000).max(1);

    let mut nodes: Vec<AxNode> = Vec::new();
    let mut truncated = false;
    for n in tree["nodes"].as_array().cloned().unwrap_or_default() {
        let ignored = n["ignored"].as_bool().unwrap_or(false);
        if ignored && !include_ignored {
            continue;
        }
        let role = n["role"]["value"].as_str().unwrap_or("").to_string();
        let name = n["name"]["value"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
        if NOISY_ROLES.contains(&role.as_str()) && name.is_none() && !include_ignored {
            continue;
        }
        if (role.is_empty() || role == "none") && name.is_none() && !include_ignored {
            continue;
        }
        let backend = n["backendDOMNodeId"].as_i64();
        let box_ = backend.and_then(|b| bounds.get(&b)).copied();
        let bounds_px = match box_ {
            Some([x, y, w, h]) => {
                let dx = (x - scroll_x) * vp.density;
                let dy = (y - scroll_y) * vp.density;
                [round1(dx), round1(dy), round1(w * vp.density), round1(h * vp.density)]
            }
            None => [0.0, 0.0, 0.0, 0.0],
        };
        let prop = |name: &str| -> Option<&Value> {
            n["properties"]
                .as_array()
                .and_then(|ps| ps.iter().find(|p| p["name"] == name))
                .map(|p| &p["value"]["value"])
        };
        let flag = |name: &str| prop(name).and_then(Value::as_bool).unwrap_or(false);
        let focusable = flag("focusable");
        let clickable = CLICKABLE_ROLES.contains(&role.as_str()) || (focusable && name.is_some());
        let node = AxNode {
            node_id: n["nodeId"].as_i64().unwrap_or(0),
            parent_id: n["parentId"].as_i64(),
            role: role.clone(),
            name: name.clone(),
            value: n["value"]["value"].as_str().map(str::to_string),
            description: n["description"]["value"].as_str().map(str::to_string),
            bounds: bounds_px,
            has_bounds: box_.is_some(),
            clickable,
            editable: flag("editable") || role == "textbox" || role == "searchbox",
            focused: flag("focused"),
            checked: prop("checked").and_then(|v| match v {
                Value::Bool(b) => Some(if *b { "true".into() } else { "false".into() }),
                Value::String(s) => Some(s.clone()),
                _ => None,
            }),
            disabled: flag("disabled"),
            backend_dom_node_id: backend,
        };
        nodes.push(node);
        if nodes.len() >= limit {
            truncated = true;
            break;
        }
    }
    let count = nodes.len();
    Ok(json!({"count": count, "truncated": truncated, "nodes": nodes}))
}

/// `backendNodeId -> [x, y, w, h]` in document CSS pixels, for the main frame.
fn layout_bounds(view: &View) -> Result<HashMap<i64, [f64; 4]>> {
    let snap = view.call(
        "DOMSnapshot.captureSnapshot",
        json!({"computedStyles": [], "includePaintOrder": false, "includeDOMRects": true}),
        Duration::from_secs(30),
    )?;
    let mut out = HashMap::new();
    let Some(docs) = snap["documents"].as_array() else {
        return Ok(out);
    };
    for doc in docs {
        let backend = doc["nodes"]["backendNodeId"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let idx = doc["layout"]["nodeIndex"].as_array().cloned().unwrap_or_default();
        let boxes = doc["layout"]["bounds"].as_array().cloned().unwrap_or_default();
        for (i, node_index) in idx.iter().enumerate() {
            let (Some(node_index), Some(b)) = (node_index.as_u64(), boxes.get(i)) else {
                continue;
            };
            let Some(bid) = backend.get(node_index as usize).and_then(Value::as_i64) else {
                continue;
            };
            let rect = b.as_array().cloned().unwrap_or_default();
            if rect.len() < 4 {
                continue;
            }
            let f = |i: usize| rect[i].as_f64().unwrap_or(0.0);
            out.insert(bid, [f(0), f(1), f(2), f(3)]);
        }
    }
    Ok(out)
}

fn scroll_offset(view: &View) -> Result<(f64, f64)> {
    let m = view.call("Page.getLayoutMetrics", json!({}), Duration::from_secs(10))?;
    let vv = &m["visualViewport"];
    Ok((
        vv["pageX"].as_f64().unwrap_or(0.0),
        vv["pageY"].as_f64().unwrap_or(0.0),
    ))
}

/// Find a node by its visible text (exact first, then substring), which is how
/// an agent taps web content. Returns the node and the centre of its box in
/// display coordinates.
pub fn find_by_text(nodes: &[Value], text: &str) -> Option<(Value, f64, f64)> {
    let needle = text.trim().to_lowercase();
    let score = |n: &Value| -> Option<i32> {
        let name = n["name"].as_str()?.trim().to_lowercase();
        if name == needle {
            Some(0)
        } else if name.contains(&needle) {
            Some(1)
        } else {
            None
        }
    };
    let mut best: Option<(i32, Value)> = None;
    for n in nodes {
        if !n["has_bounds"].as_bool().unwrap_or(false) {
            continue;
        }
        let Some(s) = score(n) else { continue };
        // Prefer interactive roles, then document order.
        let bonus = if n["clickable"].as_bool().unwrap_or(false) { 0 } else { 2 };
        let total = s + bonus;
        if best.as_ref().map(|(b, _)| total < *b).unwrap_or(true) {
            best = Some((total, n.clone()));
        }
    }
    let (_, node) = best?;
    let b = node["bounds"].as_array()?;
    let x = b.first()?.as_f64()?;
    let y = b.get(1)?.as_f64()?;
    let w = b.get(2)?.as_f64()?;
    let h = b.get(3)?.as_f64()?;
    Some((node.clone(), x + w / 2.0, y + h / 2.0))
}
