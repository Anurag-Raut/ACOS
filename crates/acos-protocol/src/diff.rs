//! FB3: what an input action changed — a settled tree diff.
//!
//! An agent that taps a control normally needs a second call (`acos_observe_tree`)
//! to learn what the tap did. This module computes the answer from two trees the
//! session already holds (the tree bound before the action and the tree the
//! action's settle produced), so the reply costs no extra traversal and no extra
//! guest round trip.
//!
//! # Identity rule
//!
//! Nodes are matched by their **stable id path**, never by list position:
//!
//! * a path segment is the node's Android `resource_id` when it has one. A named
//!   node is therefore stable when a *different* sibling is added or removed;
//! * a node without a resource id is keyed by its fully qualified `class` plus
//!   its ordinal among same-class unnamed siblings, so three anonymous
//!   `TextView`s stay distinct;
//! * segments are joined root-to-leaf with `/`, and a duplicate identity inside
//!   one tree is disambiguated with a `~N` suffix in document order.
//!
//! So `FrameLayout#0/dev.acos.fixtures.counter:id/count` keeps pointing at the
//! counter's number TextView when a sibling is inserted anywhere in the tree,
//! while a purely positional match would shift. A node that really moved keeps a
//! different identity and is reported as removed + added, which is the honest
//! answer.
//!
//! # Bound
//!
//! Each list is capped at [`MAX_DIFF_NODES`] entries; `counts` always reports the
//! true totals and `truncated` says a list was cut, so a capping reply can never
//! be read as "that was everything". This keeps an action reply inside the MCP
//! reply bound (REL3b) even for a whole-screen navigation.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ui::{Bounds, UiNode, UiTree};

/// Maximum entries returned per `added`/`removed`/`changed` list.
pub const MAX_DIFF_NODES: usize = 40;

/// One node, as an action reply reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRef {
    /// Tree-local node id of the snapshot this entry came from (`n7`).
    pub id: String,
    /// Visible text (empty when absent).
    pub text: String,
    /// Accessibility content description (empty when absent).
    pub content_desc: String,
    /// Fully qualified view class.
    pub class: String,
    /// Screen bounds in pixels.
    pub bounds: Bounds,
}

impl NodeRef {
    fn of(node: &UiNode) -> Self {
        NodeRef {
            id: node.id.clone(),
            text: node.text.clone(),
            content_desc: node.content_desc.clone(),
            class: node.class.clone(),
            bounds: node.bounds,
        }
    }
}

/// One field of a matched node that differs, old → new.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldChange {
    /// The value before the action.
    pub old: Value,
    /// The value after the action.
    pub new: Value,
}

/// A node that exists before and after the action with different state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedNode {
    /// The node after the action (id/text/content_desc/class/bounds).
    #[serde(flatten)]
    pub node: NodeRef,
    /// Which fields changed, `old` → `new`. Never empty.
    pub fields: BTreeMap<String, FieldChange>,
}

/// True totals of each list, before the [`MAX_DIFF_NODES`] cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ChangeCounts {
    /// Nodes present after the action only.
    pub added: usize,
    /// Nodes present before the action only.
    pub removed: usize,
    /// Nodes present in both whose state differs.
    pub changed: usize,
}

/// Frame sequence numbers the two trees were bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiffFrames {
    /// The pre-action frame the baseline tree belonged to (`0` = no baseline).
    pub before: u64,
    /// The settled post-action frame.
    pub after: u64,
}

/// What an input action changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeChanges {
    /// `"session"` when the diff is against the session's last observed tree,
    /// `"none"` when the session held no pre-action tree (no diff is invented).
    pub baseline: String,
    /// The two frames the comparison used.
    pub frames: DiffFrames,
    /// The activity the session reports, after the action (empty when unknown).
    pub activity: String,
    /// The focused node after the action, when one has focus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused: Option<NodeRef>,
    /// Nodes that only exist after the action (document order).
    pub added: Vec<NodeRef>,
    /// Nodes that only existed before the action (document order).
    pub removed: Vec<NodeRef>,
    /// Nodes in both trees whose state changed (after-document order).
    pub changed: Vec<ChangedNode>,
    /// True totals of `added`/`removed`/`changed`.
    pub counts: ChangeCounts,
    /// True when a list was cut at [`MAX_DIFF_NODES`].
    pub truncated: bool,
}

impl TreeChanges {
    /// The reply for an action with no pre-action tree to compare against.
    ///
    /// `baseline: "none"` and empty lists: a diff is never invented from one
    /// snapshot.
    #[must_use]
    pub fn none(frames: DiffFrames, activity: &str) -> Self {
        TreeChanges {
            baseline: "none".to_string(),
            frames,
            activity: activity.to_string(),
            focused: None,
            added: Vec::new(),
            removed: Vec::new(),
            changed: Vec::new(),
            counts: ChangeCounts::default(),
            truncated: false,
        }
    }

    /// Diff `after` against `before`.
    ///
    /// `before` is `None` when the session has no observed tree to compare with;
    /// the reply then says `baseline: "none"` and lists nothing rather than
    /// inventing an all-added diff.
    #[must_use]
    pub fn diff(
        before: Option<&UiTree>,
        after: &UiTree,
        frames: DiffFrames,
        activity: &str,
    ) -> Self {
        let focused = after.nodes.iter().find_map(find_focused).map(NodeRef::of);
        let Some(before) = before else {
            return TreeChanges::none(frames, activity);
        };
        let before_nodes = identity_map(before);
        let after_nodes = identity_map(after);
        // Indexed by identity path: a lookup per node, not a scan of the other
        // tree, so the cost stays linear in the tree size on the input hot path.
        let before_index: HashMap<&str, &UiNode> = before_nodes
            .iter()
            .map(|(key, node)| (key.as_str(), *node))
            .collect();
        let after_index: HashMap<&str, &UiNode> = after_nodes
            .iter()
            .map(|(key, node)| (key.as_str(), *node))
            .collect();
        let mut added = Vec::new();
        for (key, node) in &after_nodes {
            if !before_index.contains_key(key.as_str()) {
                added.push(NodeRef::of(node));
            }
        }
        let mut removed = Vec::new();
        for (key, node) in &before_nodes {
            if !after_index.contains_key(key.as_str()) {
                removed.push(NodeRef::of(node));
            }
        }
        let mut changed = Vec::new();
        for (key, node) in &after_nodes {
            let Some(old) = before_index.get(key.as_str()) else {
                continue;
            };
            let fields = field_changes(old, node);
            if fields.is_empty() {
                continue;
            }
            changed.push(ChangedNode {
                node: NodeRef::of(node),
                fields,
            });
        }
        let counts = ChangeCounts {
            added: added.len(),
            removed: removed.len(),
            changed: changed.len(),
        };
        let truncated = counts.added > MAX_DIFF_NODES
            || counts.removed > MAX_DIFF_NODES
            || counts.changed > MAX_DIFF_NODES;
        added.truncate(MAX_DIFF_NODES);
        removed.truncate(MAX_DIFF_NODES);
        changed.truncate(MAX_DIFF_NODES);
        TreeChanges {
            baseline: "session".to_string(),
            frames,
            activity: activity.to_string(),
            focused,
            added,
            removed,
            changed,
            counts,
            truncated,
        }
    }

    /// Did the action change the tree at all?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.counts.added == 0 && self.counts.removed == 0 && self.counts.changed == 0
    }
}

fn find_focused(node: &UiNode) -> Option<&UiNode> {
    if node.focused {
        return Some(node);
    }
    node.children.iter().find_map(find_focused)
}

/// Identity paths of every node, in document order, duplicates disambiguated.
///
/// A named node (`resource_id` set) is keyed by that id alone: inserting or
/// removing a *different* sibling cannot shift it. An unnamed node is keyed by
/// its class plus its ordinal among same-class unnamed siblings, which is the
/// only thing that tells three anonymous `TextView`s apart.
fn identity_map(tree: &UiTree) -> Vec<(String, &UiNode)> {
    let mut out = Vec::new();
    let mut taken = HashSet::new();
    walk_siblings(&tree.nodes, "", &mut out, &mut taken);
    out
}

fn walk_siblings<'a>(
    siblings: &'a [UiNode],
    prefix: &str,
    out: &mut Vec<(String, &'a UiNode)>,
    taken: &mut HashSet<String>,
) {
    let mut anonymous_ordinal: BTreeMap<&str, usize> = BTreeMap::new();
    for node in siblings {
        let segment = if node.resource_id.is_empty() {
            let ordinal = anonymous_ordinal.entry(node.class.as_str()).or_insert(0);
            let segment = format!("{}#{ordinal}", node.class);
            *ordinal += 1;
            segment
        } else {
            node.resource_id.clone()
        };
        let path = format!("{prefix}{segment}");
        // A duplicate identity (the same resource id twice under one parent)
        // is disambiguated in document order, never silently merged.
        let mut key = path.clone();
        let mut suffix = 1usize;
        while taken.contains(&key) {
            suffix += 1;
            key = format!("{path}~{suffix}");
        }
        taken.insert(key.clone());
        out.push((key, node));
        let child_prefix = format!("{path}/");
        walk_siblings(&node.children, &child_prefix, out, taken);
    }
}

/// Which compared fields differ, `old` → `new`.
fn field_changes(old: &UiNode, new: &UiNode) -> BTreeMap<String, FieldChange> {
    let mut fields = BTreeMap::new();
    let mut text = |name: &str, before: String, after: String| {
        if before != after {
            fields.insert(
                name.to_string(),
                FieldChange {
                    old: Value::String(before),
                    new: Value::String(after),
                },
            );
        }
    };
    text("text", old.text.clone(), new.text.clone());
    text(
        "content_desc",
        old.content_desc.clone(),
        new.content_desc.clone(),
    );
    text("hint", old.hint.clone(), new.hint.clone());
    text("class", old.class.clone(), new.class.clone());
    text(
        "resource_id",
        old.resource_id.clone(),
        new.resource_id.clone(),
    );
    if old.bounds != new.bounds {
        fields.insert(
            "bounds".to_string(),
            FieldChange {
                old: serde_json::json!(old.bounds.0),
                new: serde_json::json!(new.bounds.0),
            },
        );
    }
    if old.visible_bounds != new.visible_bounds {
        fields.insert(
            "visible_bounds".to_string(),
            FieldChange {
                old: serde_json::json!(old.visible_bounds.map(|b| b.0)),
                new: serde_json::json!(new.visible_bounds.map(|b| b.0)),
            },
        );
    }
    let mut flag = |name: &str, before: bool, after: bool| {
        if before != after {
            fields.insert(
                name.to_string(),
                FieldChange {
                    old: serde_json::json!(before),
                    new: serde_json::json!(after),
                },
            );
        }
    };
    flag("visible", old.visible, new.visible);
    flag("enabled", old.enabled, new.enabled);
    flag("focused", old.focused, new.focused);
    flag("clickable", old.clickable, new.clickable);
    flag("scrollable", old.scrollable, new.scrollable);
    flag("checked", old.checked, new.checked);
    flag("selected", old.selected, new.selected);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, class: &str, resource_id: &str, text: &str) -> UiNode {
        UiNode::new(id, class, Bounds::new(0, 0, 10, 10))
            .with_resource_id(resource_id)
            .with_text(text)
    }

    fn tree(nodes: Vec<UiNode>) -> UiTree {
        UiTree {
            frame: crate::frame::FrameMeta {
                frame_seq: 1,
                produced_at: String::new(),
                cause_id: String::new(),
                width: 1080,
                height: 2400,
                density: 420,
                content_hash: String::new(),
                meaningful: true,
                display_origin: None,
                display_size: None,
            },
            package: "dev.acos.test".to_string(),
            nodes,
            truncated: false,
            text_available: true,
            windows: Vec::new(),
        }
    }

    /// Counter-shaped tree: a root with the number and the button.
    fn counter_tree(count: &str, button_focused: bool) -> UiTree {
        let mut number = node("n1", "android.widget.TextView", "app:id/count", count);
        number.bounds = Bounds::new(42, 317, 63, 368);
        let mut button = node("n2", "android.widget.Button", "app:id/inc", "Increment");
        button.bounds = Bounds::new(42, 368, 273, 494);
        button.clickable = true;
        button.focused = button_focused;
        let root =
            node("n0", "android.widget.FrameLayout", "", "").with_children(vec![number, button]);
        tree(vec![root])
    }

    #[test]
    fn one_text_change_is_the_only_change() {
        let before = counter_tree("0", true);
        let after = counter_tree("1", true);
        let changes = TreeChanges::diff(
            Some(&before),
            &after,
            DiffFrames {
                before: 7,
                after: 9,
            },
            "dev.acos.test.Main",
        );
        assert_eq!(changes.baseline, "session");
        assert_eq!(
            changes.counts,
            ChangeCounts {
                added: 0,
                removed: 0,
                changed: 1
            }
        );
        assert!(changes.added.is_empty());
        assert!(changes.removed.is_empty());
        let entry = &changes.changed[0];
        assert_eq!(entry.node.id, "n1");
        assert_eq!(entry.node.text, "1");
        assert_eq!(entry.node.class, "android.widget.TextView");
        assert_eq!(entry.node.bounds, Bounds::new(42, 317, 63, 368));
        assert_eq!(entry.fields.len(), 1);
        let text = &entry.fields["text"];
        assert_eq!(text.old, Value::String("0".into()));
        assert_eq!(text.new, Value::String("1".into()));
        assert!(!changes.truncated);
        assert_eq!(changes.activity, "dev.acos.test.Main");
        assert_eq!(changes.focused.as_ref().unwrap().id, "n2");
    }

    #[test]
    fn added_and_removed_nodes_are_matched_by_id_path_not_position() {
        // Before: [root -> header, body]; after: [root -> body] and the body's
        // own child list gained one node. A positional match would report the
        // header as "changed into body"; the id path reports a removal.
        let header = node("n1", "android.widget.TextView", "app:id/header", "Header");
        let body = node("n2", "android.widget.FrameLayout", "app:id/body", "")
            .with_children(vec![node("n3", "android.widget.TextView", "", "one")]);
        let root =
            node("n0", "android.widget.LinearLayout", "", "").with_children(vec![header, body]);
        let before = tree(vec![root]);

        let body = node("n1", "android.widget.FrameLayout", "app:id/body", "").with_children(vec![
            node("n2", "android.widget.TextView", "", "one"),
            node("n3", "android.widget.TextView", "app:id/extra", "two"),
        ]);
        let root = node("n0", "android.widget.LinearLayout", "", "").with_children(vec![body]);
        let after = tree(vec![root]);

        let changes = TreeChanges::diff(
            Some(&before),
            &after,
            DiffFrames {
                before: 1,
                after: 2,
            },
            "",
        );
        assert_eq!(changes.counts.removed, 1, "{changes:#?}");
        assert_eq!(changes.removed[0].id, "n1");
        assert_eq!(changes.removed[0].text, "Header");
        // `body` keeps its resource id, so it is matched (same identity path),
        // and the added TextView is the only addition.
        assert_eq!(changes.counts.added, 1, "{changes:#?}");
        assert_eq!(changes.added[0].text, "two");
        assert_eq!(changes.counts.changed, 0, "{changes:#?}");
    }

    #[test]
    fn flag_and_bounds_changes_carry_old_and_new() {
        let before = counter_tree("0", false);
        let mut after = counter_tree("0", true);
        after.nodes[0].children[1].bounds = Bounds::new(42, 368, 273, 500);
        let changes = TreeChanges::diff(
            Some(&before),
            &after,
            DiffFrames {
                before: 3,
                after: 4,
            },
            "",
        );
        assert_eq!(changes.counts.changed, 1);
        let fields = &changes.changed[0].fields;
        assert_eq!(fields["focused"].old, Value::Bool(false));
        assert_eq!(fields["focused"].new, Value::Bool(true));
        assert_eq!(fields["bounds"].old, serde_json::json!([42, 368, 273, 494]));
        assert_eq!(fields["bounds"].new, serde_json::json!([42, 368, 273, 500]));
    }

    #[test]
    fn lists_are_capped_and_say_so() {
        let before = tree(vec![node("n0", "android.widget.FrameLayout", "", "")]);
        let children: Vec<UiNode> = (0..(MAX_DIFF_NODES + 5))
            .map(|i| node(&format!("n{i}"), "android.widget.TextView", "", "x"))
            .collect();
        let after = tree(vec![
            node("n0", "android.widget.FrameLayout", "", "").with_children(children)
        ]);
        let changes = TreeChanges::diff(
            Some(&before),
            &after,
            DiffFrames {
                before: 1,
                after: 2,
            },
            "",
        );
        assert!(changes.truncated);
        assert_eq!(changes.counts.added, MAX_DIFF_NODES + 5);
        assert_eq!(changes.added.len(), MAX_DIFF_NODES);
    }

    #[test]
    fn no_baseline_invents_nothing() {
        let after = counter_tree("1", false);
        let changes = TreeChanges::diff(
            None,
            &after,
            DiffFrames {
                before: 0,
                after: 5,
            },
            "dev.acos.test.Main",
        );
        assert_eq!(changes.baseline, "none");
        assert!(changes.is_empty());
        assert!(
            changes.added.is_empty() && changes.removed.is_empty() && changes.changed.is_empty()
        );
        assert_eq!(changes.frames.after, 5);
        assert_eq!(changes.activity, "dev.acos.test.Main");
        assert_eq!(
            serde_json::to_value(&changes).unwrap()["baseline"],
            Value::String("none".into())
        );
    }

    #[test]
    fn identical_trees_report_no_change() {
        let before = counter_tree("0", true);
        let changes = TreeChanges::diff(Some(&before), &before, DiffFrames::default(), "");
        assert!(changes.is_empty());
        assert!(!changes.truncated);
        assert!(changes.focused.is_some());
    }

    #[test]
    fn duplicate_identities_stay_distinct() {
        // Three anonymous TextViews with no resource id: only the id path
        // (class + sibling index) tells them apart.
        let before = tree(vec![node("n0", "android.widget.LinearLayout", "", "")
            .with_children(vec![
                node("n1", "android.widget.TextView", "", "a"),
                node("n2", "android.widget.TextView", "", "b"),
            ])]);
        let after = tree(vec![node("n0", "android.widget.LinearLayout", "", "")
            .with_children(vec![
                node("n1", "android.widget.TextView", "", "a"),
                node("n2", "android.widget.TextView", "", "c"),
            ])]);
        let changes = TreeChanges::diff(Some(&before), &after, DiffFrames::default(), "");
        assert_eq!(changes.counts.changed, 1);
        assert_eq!(changes.changed[0].node.text, "c");
    }

    #[test]
    fn pure_addition_reports_only_added() {
        let before = counter_tree("0", false);
        let mut after = counter_tree("0", false);
        after.nodes[0].children.push(node(
            "n9",
            "android.widget.TextView",
            "app:id/toast",
            "Saved",
        ));
        let changes = TreeChanges::diff(Some(&before), &after, DiffFrames::default(), "");
        assert_eq!(
            changes.counts,
            ChangeCounts {
                added: 1,
                removed: 0,
                changed: 0
            }
        );
        assert_eq!(changes.added[0].text, "Saved");
        assert_eq!(changes.added[0].id, "n9");
        assert!(changes.removed.is_empty() && changes.changed.is_empty());
        assert!(!changes.truncated);
    }

    #[test]
    fn pure_removal_reports_only_removed() {
        let before = counter_tree("0", false);
        let mut after = counter_tree("0", false);
        after.nodes[0]
            .children
            .retain(|child| child.resource_id != "app:id/inc");
        let changes = TreeChanges::diff(Some(&before), &after, DiffFrames::default(), "");
        assert_eq!(
            changes.counts,
            ChangeCounts {
                added: 0,
                removed: 1,
                changed: 0
            }
        );
        assert_eq!(changes.removed[0].text, "Increment");
        assert_eq!(changes.removed[0].id, "n2");
        assert!(changes.added.is_empty() && changes.changed.is_empty());
    }

    #[test]
    fn removed_and_changed_lists_are_capped_with_true_counts() {
        let make = |text: &str| {
            let children: Vec<UiNode> = (0..(MAX_DIFF_NODES + 3))
                .map(|i| node(&format!("c{i}"), "android.widget.TextView", "", text))
                .collect();
            tree(vec![
                node("n0", "android.widget.FrameLayout", "", "").with_children(children)
            ])
        };
        let before = make("old");
        let after = make("new");
        let changes = TreeChanges::diff(Some(&before), &after, DiffFrames::default(), "");
        assert_eq!(changes.counts.changed, MAX_DIFF_NODES + 3);
        assert_eq!(changes.changed.len(), MAX_DIFF_NODES);
        assert_eq!(
            changes.changed[0].fields["text"].old,
            Value::String("old".into())
        );
        assert!(changes.truncated);

        let empty = tree(vec![node("n0", "android.widget.FrameLayout", "", "")]);
        let removed = TreeChanges::diff(Some(&before), &empty, DiffFrames::default(), "");
        assert_eq!(removed.counts.removed, MAX_DIFF_NODES + 3);
        assert_eq!(removed.removed.len(), MAX_DIFF_NODES);
        assert!(removed.truncated);
    }
}
