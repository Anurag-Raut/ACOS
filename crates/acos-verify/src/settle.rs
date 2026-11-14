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
//! The signature is a hash over **every field a selector or an assertion can
//! read** ([`SIGNATURE_FIELDS`]), in tree/visible order, plus the tree-level
//! `package`, `truncated` and `text_available` facts: `class`, `resource_id`,
//! `text`, `content_desc`, `hint`, `bounds`, `visible_bounds`, `visible`,
//! `enabled`, `focused`, `clickable`, `scrollable`, `checked`, `selected`,
//! `window`, `package` and the node structure/order. Nothing is silently
//! dropped from that list, so a field an assertion could read can never change
//! while the signature stays the same. A text value that an animation updates
//! (a countdown, a progress percentage) therefore *does* change the signature:
//! the screen is not content-stable, and that is reported as a `TIMEOUT` rather
//! than mistaken for a settled observation.

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

/// Every node field [`content_signature`] hashes: exactly the fields a
/// selector (`Selector::matches_node`) or an assertion (`assert_text`,
/// `assert_visible`, `assert_state`, `assert_offscreen`, ...) can read, plus the
/// structural facts that decide whether a read is even meaningful. A field an
/// assertion reads may never change without changing the signature.
pub const SIGNATURE_FIELDS: [&str; 16] = [
    "class",
    "resource_id",
    "text",
    "content_desc",
    "hint",
    "bounds",
    "visible_bounds",
    "visible",
    "enabled",
    "focused",
    "clickable",
    "scrollable",
    "checked",
    "selected",
    "window",
    "package",
];

/// Tree-level facts whose change invalidates a content verdict (a truncated
/// tree or a missing text route means the tree did not describe the screen).
pub const SIGNATURE_TREE_FIELDS: [&str; 3] = ["package", "truncated", "text_available"];

/// `details.reason` of a content settle that ran out of time because the
/// signature kept changing (the screen is simply not content-stable).
pub const CONTENT_NOT_STABLE: &str = "content-not-stable";

/// `details.reason` of a content settle that could not prove its traversals
/// were fresh, distinct samples (a stale probe sample is never evidence).
pub const CONTENT_PROBE_UNPROVEN: &str = "content-probe-unproven";

/// Hash one node subtree in visible (tree) order.
fn hash_node<H: std::hash::Hasher>(node: &UiNode, hasher: &mut H) {
    use std::hash::Hash;
    node.class.hash(hasher);
    node.resource_id.hash(hasher);
    node.text.hash(hasher);
    node.content_desc.hash(hasher);
    node.hint.hash(hasher);
    node.checked.hash(hasher);
    node.selected.hash(hasher);
    node.enabled.hash(hasher);
    node.focused.hash(hasher);
    node.clickable.hash(hasher);
    node.scrollable.hash(hasher);
    node.visible.hash(hasher);
    node.bounds.0.hash(hasher);
    // W58: the clipped rect is what a tap/`assert_offscreen` reads.
    node.visible_bounds.map(|b| b.0).hash(hasher);
    // GR4: the window label decides which frame a bounds value refers to.
    node.window.hash(hasher);
    node.package.hash(hasher);
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
    tree.package.hash(&mut hasher);
    tree.truncated.hash(&mut hasher);
    tree.text_available.hash(&mut hasher);
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
    /// The signature of the current window generation.
    pub fn signature(&self) -> Option<u64> {
        self.signature
    }

    /// Traversals observed with the current signature.
    pub fn traversals(&self) -> u32 {
        self.traversals
    }

    /// The window this state machine requires, in milliseconds.
    /// The window this settle requires, in milliseconds.
    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }
}

/// What one traversal told the content-settle loop to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentAttempt {
    /// Keep traversing: the window is not satisfied yet.
    Continue,
    /// The same signature has now been seen across enough *fresh, distinct*
    /// traversals spanning the window.
    Settled,
    /// The attempt is over. `&'static str` is the `details.reason`:
    /// [`CONTENT_NOT_STABLE`] or [`CONTENT_PROBE_UNPROVEN`].
    TimedOut(&'static str),
}

/// The host-side content window **plus the freshness proof** for the traversals
/// it consumes.
///
/// A content verdict is only as good as its samples. Two traps this type
/// closes (both were real in the first SE2 landing):
///
/// * a tree reply can carry the *same* probe sample again (the guest returns
///   its latest sample when a request times out), so two "traversals" can be one
///   observation. [`Self::observe`] counts a traversal only when its
///   `probe_seq` strictly increased, so a repeated sample never satisfies
///   [`MIN_CONTENT_TRAVERSALS`] and is refused as unproven.
/// * a sampler that predates the request (or the input being settled) can make
///   a busy UI thread look stable on the *departing* screen. `floor_probe_seq`
///   is the newest sample bound before the window started; a reply at or below
///   it is [`ContentAttempt::TimedOut`] with [`CONTENT_PROBE_UNPROVEN`], and a
///   reply with no `probe_seq` at all is refused the same way (fail closed).
#[derive(Debug, Clone)]
pub struct ContentProbeWindow {
    window: ContentWindow,
    floor_probe_seq: u64,
    first_probe_seq: Option<u64>,
    last_probe_seq: Option<u64>,
    fresh_traversals: u32,
    unproven: u32,
}

impl ContentProbeWindow {
    /// `floor_probe_seq` is the newest probe sample that existed before this
    /// window began (0 when the session has none). Every consumed traversal must
    /// be strictly newer.
    pub fn new(window_ms: u64, floor_probe_seq: u64) -> Self {
        Self {
            window: ContentWindow::new(window_ms),
            floor_probe_seq,
            first_probe_seq: None,
            last_probe_seq: None,
            fresh_traversals: 0,
            unproven: 0,
        }
    }

    /// The `min_probe_seq` the *next* traversal must beat: the last consumed
    /// sample, or the floor before the first one.
    pub fn next_floor(&self) -> u64 {
        self.last_probe_seq.unwrap_or(self.floor_probe_seq)
    }

    /// Classify one reply without consuming it. `None` = the reply carried no
    /// `probe_seq`, which cannot be proven fresh.
    pub fn classify(&self, probe_seq: Option<u64>) -> ProbeVerdict {
        let Some(seq) = probe_seq else {
            return ProbeVerdict::Unproven;
        };
        if seq <= self.next_floor() {
            return ProbeVerdict::Unproven;
        }
        ProbeVerdict::Fresh(seq)
    }

    /// Feed one traversal and decide the loop's next move.
    ///
    /// The deadline is checked on **every** traversal, independently of the
    /// window state: a screen whose signature changes on every traversal resets
    /// the window (and its traversal count) forever, and that must still end in
    /// [`ContentAttempt::TimedOut`] rather than spin until an outer wall clock
    /// kills the run.
    pub fn attempt(
        &mut self,
        signature: u64,
        probe_seq: Option<u64>,
        at_ms: u64,
        deadline_ms: u64,
    ) -> ContentAttempt {
        match self.classify(probe_seq) {
            ProbeVerdict::Unproven => {
                self.unproven += 1;
                return ContentAttempt::TimedOut(CONTENT_PROBE_UNPROVEN);
            }
            ProbeVerdict::Fresh(seq) => {
                self.last_probe_seq = Some(seq);
                self.first_probe_seq.get_or_insert(seq);
                self.fresh_traversals += 1;
            }
        }
        let settled = self.window.observe(signature, at_ms);
        if settled {
            return ContentAttempt::Settled;
        }
        if at_ms >= deadline_ms {
            return ContentAttempt::TimedOut(CONTENT_NOT_STABLE);
        }
        ContentAttempt::Continue
    }

    /// Classify a reply that is *not* a window traversal (the pixel-bound
    /// binding traversal): it must not be older than the last consumed sample,
    /// and it must show the settled signature.
    pub fn binds_settled_content(&self, signature: u64, probe_seq: Option<u64>) -> bool {
        let Some(seq) = probe_seq else {
            return false;
        };
        seq >= self.next_floor()
            && self.window.signature() == Some(signature)
            && self.window.traversals() >= MIN_CONTENT_TRAVERSALS
    }

    /// Fresh, distinct traversals consumed by the current window generation.
    pub fn fresh_traversals(&self) -> u32 {
        self.fresh_traversals
    }

    /// Distinct fresh traversals spent on a *previous* signature (they do not
    /// count toward the current window, and say so in the evidence).
    pub fn traversals(&self) -> u32 {
        self.window.traversals()
    }

    /// The newest probe sample consumed by the window.
    pub fn last_probe_seq(&self) -> Option<u64> {
        self.last_probe_seq
    }

    /// The first probe sample consumed by the window (its freshness proof).
    pub fn first_probe_seq(&self) -> Option<u64> {
        self.first_probe_seq
    }

    /// Replies refused because their `probe_seq` was absent or stale.
    pub fn unproven(&self) -> u32 {
        self.unproven
    }

    /// The pre-window sample the first traversal had to beat.
    pub fn floor_probe_seq(&self) -> u64 {
        self.floor_probe_seq
    }

    /// The content window being proven.
    pub fn window(&self) -> &ContentWindow {
        &self.window
    }

    /// The window this settle requires, in milliseconds.
    pub fn window_ms(&self) -> u64 {
        self.window.window_ms()
    }

    /// The signature of the current window generation.
    pub fn signature(&self) -> Option<u64> {
        self.window.signature()
    }
}

/// Classification of one tree reply's `probe_seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeVerdict {
    /// A sample strictly newer than everything already consumed.
    Fresh(u64),
    /// No `probe_seq`, or one at/below the window's floor.
    Unproven,
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

    /// SE2b finding 1: a sample that predates the window (or the request) can
    /// never be one of its traversals. Before the fix the loop fed whatever
    /// `latest_probe` returned into the window, so a pre-request sample could
    /// "settle" the departing screen.
    #[test]
    fn a_stale_probe_sample_never_counts_toward_the_window() {
        let mut window = ContentProbeWindow::new(300, 10);
        // At/below the floor: the pre-window sample, reused.
        assert_eq!(window.classify(Some(10)), ProbeVerdict::Unproven);
        assert_eq!(window.classify(Some(9)), ProbeVerdict::Unproven);
        assert_eq!(
            window.attempt(7, Some(10), 0, 10_000),
            ContentAttempt::TimedOut(CONTENT_PROBE_UNPROVEN)
        );
        assert_eq!(window.fresh_traversals(), 0);
        assert_eq!(window.unproven(), 1);
    }

    /// SE2b finding 1: no `probe_seq` at all is unprovable freshness, so the
    /// settle fails closed instead of trusting the tree.
    #[test]
    fn a_reply_without_probe_seq_fails_closed() {
        let mut window = ContentProbeWindow::new(300, 0);
        assert_eq!(window.attempt(7, None, 0, 10_000), ContentAttempt::TimedOut(CONTENT_PROBE_UNPROVEN));
        assert_eq!(window.fresh_traversals(), 0);
    }

    /// SE2b finding 1: the same sample seen twice is one traversal, never two.
    #[test]
    fn a_repeated_probe_sample_does_not_count_as_a_distinct_traversal() {
        let mut window = ContentProbeWindow::new(300, 0);
        assert_eq!(window.attempt(7, Some(11), 0, 10_000), ContentAttempt::Continue);
        assert_eq!(window.attempt(7, Some(11), 150, 10_000), ContentAttempt::TimedOut(CONTENT_PROBE_UNPROVEN));
        assert_eq!(window.fresh_traversals(), 1, "a repeated sample is not a second traversal");
        // A genuinely newer sample continues the same window.
        let mut window = ContentProbeWindow::new(300, 0);
        assert_eq!(window.attempt(7, Some(11), 0, 10_000), ContentAttempt::Continue);
        assert_eq!(window.attempt(7, Some(12), 150, 10_000), ContentAttempt::Continue);
        assert_eq!(window.attempt(7, Some(13), 300, 10_000), ContentAttempt::Settled);
    }

    /// SE2b finding 1: `next_floor` advances with each consumed sample, so the
    /// request can tell the guest "never hand me this one again".
    #[test]
    fn the_next_floor_is_the_last_consumed_sample() {
        let mut window = ContentProbeWindow::new(300, 4);
        assert_eq!(window.next_floor(), 4);
        assert_eq!(window.attempt(7, Some(5), 0, 10_000), ContentAttempt::Continue);
        assert_eq!(window.next_floor(), 5);
        assert_eq!(window.first_probe_seq(), Some(5));
    }

    /// SE2b finding 3: a screen whose signature changes on *every* traversal
    /// resets the window (and the traversal count) forever. The deadline must
    /// therefore be checked independently; before the fix the loop only timed out
    /// once `traversals >= 2`, which a countdown/alternating screen never reaches.
    #[test]
    fn a_signature_that_changes_every_traversal_still_times_out() {
        let mut window = ContentProbeWindow::new(300, 0);
        let mut seq = 0u64;
        let mut at = 0u64;
        let mut attempts = 0;
        loop {
            seq += 1;
            at += 100;
            attempts += 1;
            match window.attempt(seq, Some(seq), at, 1_000) {
                ContentAttempt::TimedOut(reason) => {
                    assert_eq!(reason, CONTENT_NOT_STABLE);
                    break;
                }
                ContentAttempt::Settled => panic!("an always-changing signature can never settle"),
                ContentAttempt::Continue => {
                    // The window state itself never counts past one traversal,
                    // which is exactly why the old deadline test hung.
                    assert_eq!(window.traversals(), 1);
                    assert!(attempts < 100, "the loop must time out on the deadline");
                }
            }
        }
        assert!(attempts >= 10, "the deadline, not a traversal count, ended it");
    }

    /// SE2b finding 5: every field an assertion or selector can read is hashed.
    #[test]
    fn signature_covers_every_field_an_assertion_reads() {
        let base = tree(vec![node("n1", "Welcome", Bounds::new(0, 0, 10, 10))]);
        let base_sig = content_signature(&base);
        let mut changed = Vec::new();

        let mut t = base.clone();
        t.nodes[0].resource_id = "pkg:id/title".to_string();
        changed.push(("resource_id", content_signature(&t)));
        let mut t = base.clone();
        t.nodes[0].clickable = true;
        changed.push(("clickable", content_signature(&t)));
        let mut t = base.clone();
        t.nodes[0].scrollable = true;
        changed.push(("scrollable", content_signature(&t)));
        let mut t = base.clone();
        t.nodes[0].visible_bounds = Some(Bounds::new(0, 0, 5, 5));
        changed.push(("visible_bounds", content_signature(&t)));
        let mut t = base.clone();
        t.nodes[0].window = "w1".to_string();
        changed.push(("window", content_signature(&t)));
        let mut t = base.clone();
        t.nodes[0].package = "dev.acos.fixtures".to_string();
        changed.push(("package", content_signature(&t)));
        let mut t = base.clone();
        t.truncated = true;
        changed.push(("truncated", content_signature(&t)));
        let mut t = base.clone();
        t.text_available = false;
        changed.push(("text_available", content_signature(&t)));
        let mut t = base.clone();
        t.package = "other".to_string();
        changed.push(("tree.package", content_signature(&t)));

        for (field, signature) in changed {
            assert_ne!(base_sig, signature, "a {field} change must change the signature");
        }
        // The published field list is the audited one: every field here is
        // named, so a reader can check the signature against the assertions.
        for field in [
            "class", "resource_id", "text", "content_desc", "hint", "bounds", "visible_bounds",
            "visible", "enabled", "focused", "clickable", "scrollable", "checked", "selected",
            "window", "package",
        ] {
            assert!(SIGNATURE_FIELDS.contains(&field), "{field} missing from SIGNATURE_FIELDS");
        }
        assert_eq!(SIGNATURE_TREE_FIELDS, ["package", "truncated", "text_available"]);
    }

    #[test]
    fn configured_window_is_never_below_the_floor() {
        assert_eq!(content_window_ms(Some(0)), MIN_CONTENT_WINDOW_MS);
        assert_eq!(content_window_ms(Some(100)), MIN_CONTENT_WINDOW_MS);
        assert_eq!(content_window_ms(Some(500)), 500);
        assert_eq!(content_window_ms(None), DEFAULT_CONTENT_WINDOW_MS);
    }
}
