//! SE2: content-stable settle (ADR-0026 decision 3).
//!
//! A screen can animate forever in pixels while its *content* (what the user
//! can read and what the tree describes) is stable — Gaten's welcome/onboarding
//! loop is the reference case (GR3/G3e, `results/gaten/GR3.md` §4.3). Strict
//! settle, which waits for the guest's draw/event state to go quiet, never
//! reaches a verdict there: it is an honest `TIMEOUT`.
//!
//! `settle: content` is the opt-in rule for exactly that screen. It is settled
//! when the UI tree **content signature** is unchanged across fresh traversals
//! spanning at least [`DEFAULT_CONTENT_WINDOW_MS`], with at least
//! [`MIN_CONTENT_TRAVERSALS`] traversals. Pixels are never consulted, and the
//! rule therefore never justifies a pixel/frame assertion: content stability
//! says nothing about the pixels, which may still be animating. Such an
//! assertion is failed closed by the executor (see
//! `executor::content_settle_pixel_failure`).
//!
//! The signature is a hash over, in tree/visible order, each node's
//! `(class, text, content_desc, hint, checked, selected, enabled, focused,
//! bounds, visible)` and the node structure. Nothing else is hashed — in
//! particular a resource id, a clickable/scrollable flag or a package name does
//! not make the content differ, and no field is silently dropped from that
//! list. A text value that an animation updates (a countdown, a progress
//! percentage) therefore *does* change the signature: the screen is not
//! content-stable, and that is reported as a `TIMEOUT` rather than mistaken for
//! a settled observation.

use acos_protocol::ui::{UiNode, UiTree};

/// The documented content-stable window: a tree signature must be unchanged
/// across traversals spanning at least this long (ADR-0026: "~300 ms").
pub const DEFAULT_CONTENT_WINDOW_MS: u64 = 300;

/// A content settle needs at least this many traversals, whatever the window:
/// one signature sample can never prove stability.
pub const MIN_CONTENT_TRAVERSALS: u32 = 2;

/// The lower bound a caller may configure the window to. A smaller window
/// cannot span two traversals of a real screen and would only invite a false
/// "settled".
pub const MIN_CONTENT_WINDOW_MS: u64 = DEFAULT_CONTENT_WINDOW_MS;

/// The configured window, never below the floor.
pub fn content_window_ms(requested: Option<u64>) -> u64 {
    requested.unwrap_or(DEFAULT_CONTENT_WINDOW_MS).max(MIN_CONTENT_WINDOW_MS)
}

/// Hash one node subtree in visible (tree) order.
fn hash_node<H: std::hash::Hasher>(node: &UiNode, hasher: &mut H) {
    use std::hash::Hash;
    node.class.hash(hasher);
    node.text.hash(hasher);
    node.content_desc.hash(hasher);
    node.hint.hash(hasher);
    node.checked.hash(hasher);
    node.selected.hash(hasher);
    node.enabled.hash(hasher);
    node.focused.hash(hasher);
    node.visible.hash(hasher);
    node.bounds.0.hash(hasher);
    // The child count is hashed so a subtree cannot collide with its sibling
    // sequence by concatenation alone.
    node.children.len().hash(hasher);
    for child in &node.children {
        hash_node(child, hasher);
    }
}

/// The content signature of a UI tree (SE2).
///
/// Stable across frames with the same content, independent of pixels, frame
/// numbers, timestamps and the guest's node ids. Joining the hashed fields with
/// explicit child counts keeps the order significant, so a reordered tree is a
/// different signature.
pub fn content_signature(tree: &UiTree) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tree.nodes.len().hash(&mut hasher);
    for node in &tree.nodes {
        hash_node(node, &mut hasher);
    }
    hasher.finish()
}

/// The host-side window state machine for `settle: content`.
///
/// `observe` is fed one signature per fresh traversal with the elapsed
/// millisecond timestamp of that traversal. It answers `true` only when the
/// same signature has now been seen at least [`MIN_CONTENT_TRAVERSALS`] times,
/// the first of which is at least the window old. A different signature starts
/// a new window (the change resets the span and the traversal count), so an
/// animation that keeps changing the tree can never be reported as settled.
#[derive(Debug, Clone)]
pub struct ContentWindow {
    window_ms: u64,
    signature: Option<u64>,
    since_ms: u64,
    traversals: u32,
}

impl ContentWindow {
    /// A window of at least [`MIN_CONTENT_WINDOW_MS`].
    pub fn new(window_ms: u64) -> Self {
        Self {
            window_ms: window_ms.max(MIN_CONTENT_WINDOW_MS),
            signature: None,
            since_ms: 0,
            traversals: 0,
        }
    }

    /// Feed one traversal's signature at `at_ms` (milliseconds, monotonic).
    /// Returns `true` exactly when the content is settled.
    pub fn observe(&mut self, signature: u64, at_ms: u64) -> bool {
        match self.signature {
            Some(current) if current == signature => {
                self.traversals += 1;
            }
            _ => {
                self.signature = Some(signature);
                self.since_ms = at_ms;
                self.traversals = 1;
                return false;
            }
        }
        self.traversals >= MIN_CONTENT_TRAVERSALS
            && at_ms.saturating_sub(self.since_ms) >= self.window_ms
    }

    /// The signature of the current (last observed) window.
    pub fn signature(&self) -> Option<u64> {
        self.signature
    }

    /// Traversals observed with the current signature.
    pub fn traversals(&self) -> u32 {
        self.traversals
    }

    /// The window this state machine requires, in milliseconds.
    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_protocol::ui::{Bounds, UiTree};
    use acos_protocol::FrameMeta;

    fn frame() -> FrameMeta {
        FrameMeta::new(1, "2026-01-01T00:00:00Z", "test", 100, 100, 420, "h", true)
    }

    fn node(id: &str, text: &str, bounds: Bounds) -> UiNode {
        UiNode::new(id, "android.widget.TextView", bounds).with_text(text)
    }

    fn tree(nodes: Vec<UiNode>) -> UiTree {
        UiTree::new(frame(), "pkg", nodes)
    }

    #[test]
    fn same_content_has_the_same_signature() {
        let a = tree(vec![
            node("n1", "Welcome", Bounds::new(0, 0, 10, 10)),
            node("n2", "Skip", Bounds::new(0, 10, 10, 20)),
        ]);
        // Different node ids and pixels/content hash: content is identical.
        let mut b = tree(vec![
            node("x9", "Welcome", Bounds::new(0, 0, 10, 10)),
            node("x8", "Skip", Bounds::new(0, 10, 10, 20)),
        ]);
        b.frame.content_hash = "different-pixels".to_string();
        b.frame.frame_seq = 99;
        assert_eq!(content_signature(&a), content_signature(&b));
    }

    #[test]
    fn text_bounds_and_state_changes_change_the_signature() {
        let base = tree(vec![node("n1", "Welcome", Bounds::new(0, 0, 10, 10))]);
        let base_sig = content_signature(&base);

        let text = tree(vec![node("n1", "Welcome 2", Bounds::new(0, 0, 10, 10))]);
        assert_ne!(base_sig, content_signature(&text));

        let text_anim = tree(vec![node("n1", "Welcome", Bounds::new(0, 1, 10, 11))]);
        assert_ne!(base_sig, content_signature(&text_anim));

        let mut checked = tree(vec![node("n1", "Welcome", Bounds::new(0, 0, 10, 10))]);
        checked.nodes[0].checked = true;
        assert_ne!(base_sig, content_signature(&checked));

        let mut selected = tree(vec![node("n1", "Welcome", Bounds::new(0, 0, 10, 10))]);
        selected.nodes[0].selected = true;
        assert_ne!(base_sig, content_signature(&selected));

        let mut disabled = tree(vec![node("n1", "Welcome", Bounds::new(0, 0, 10, 10))]);
        disabled.nodes[0].enabled = false;
        assert_ne!(base_sig, content_signature(&disabled));

        let mut focused = tree(vec![node("n1", "Welcome", Bounds::new(0, 0, 10, 10))]);
        focused.nodes[0].focused = true;
        assert_ne!(base_sig, content_signature(&focused));

        let desc = {
            let mut t = tree(vec![node("n1", "Welcome", Bounds::new(0, 0, 10, 10))]);
            t.nodes[0].content_desc = "intro".to_string();
            t
        };
        assert_ne!(base_sig, content_signature(&desc));

        let hint = {
            let mut t = tree(vec![node("n1", "Welcome", Bounds::new(0, 0, 10, 10))]);
            t.nodes[0].hint = "type here".to_string();
            t
        };
        assert_ne!(base_sig, content_signature(&hint));
    }

    #[test]
    fn child_structure_and_order_change_the_signature() {
        let flat = tree(vec![
            node("n1", "a", Bounds::new(0, 0, 1, 1)),
            node("n2", "b", Bounds::new(0, 1, 1, 2)),
        ]);
        let nested = tree(vec![node("n1", "a", Bounds::new(0, 0, 1, 1))
            .with_children(vec![node("n2", "b", Bounds::new(0, 1, 1, 2))])]);
        assert_ne!(content_signature(&flat), content_signature(&nested));

        let mut reversed = flat.clone();
        reversed.nodes.reverse();
        assert_ne!(content_signature(&flat), content_signature(&reversed));
    }

    #[test]
    fn window_settles_only_after_two_traversals_over_the_window() {
        let mut window = ContentWindow::new(300);
        // First sample: one traversal can never prove stability.
        assert!(!window.observe(7, 0));
        // Same content but not yet 300 ms old.
        assert!(!window.observe(7, 299));
        assert_eq!(window.traversals(), 2);
        // Same content, window satisfied by the third traversal.
        assert!(window.observe(7, 300));
    }

    #[test]
    fn window_never_settles_before_two_traversals_even_at_a_zero_window() {
        let mut window = ContentWindow::new(0);
        assert_eq!(window.window_ms(), MIN_CONTENT_WINDOW_MS);
        // One sample can never be settled, however old it is.
        assert!(!window.observe(1, 0));
        // Two samples at the same instant do not span the window either, even
        // though the traversal count is now sufficient.
        assert!(!window.observe(1, 0));
        assert!(!window.observe(1, 299));
        assert!(window.observe(1, 300));
        assert!(window.traversals() >= MIN_CONTENT_TRAVERSALS);
    }

    #[test]
    fn a_change_resets_the_window() {
        let mut window = ContentWindow::new(300);
        assert!(!window.observe(7, 0));
        assert!(!window.observe(7, 150));
        assert!(window.observe(7, 300));
        // A different tree (an animated text value) starts the window again.
        assert!(!window.observe(8, 900));
        assert_eq!(window.traversals(), 1);
        assert!(!window.observe(8, 1199));
        assert!(window.observe(8, 1200));
        // An A -> B -> A change is two resets, never a pass.
        assert!(!window.observe(9, 1300));
        assert!(!window.observe(8, 1600));
        assert!(!window.observe(9, 1900));
    }

    #[test]
    fn configured_window_is_never_below_the_floor() {
        assert_eq!(content_window_ms(Some(0)), MIN_CONTENT_WINDOW_MS);
        assert_eq!(content_window_ms(Some(100)), MIN_CONTENT_WINDOW_MS);
        assert_eq!(content_window_ms(Some(500)), 500);
        assert_eq!(content_window_ms(None), DEFAULT_CONTENT_WINDOW_MS);
    }
}
