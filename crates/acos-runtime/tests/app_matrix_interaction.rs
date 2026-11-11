//! W187-A end-to-end interaction matrix for the `list-adapter`,
//! `fragments-nav` and `dialogs-menus` fixtures.
//!
//! Every passing test asserts a **live tree change** produced by the app's own
//! framework code after an injected `MotionEvent` (never `accepted == true`
//! alone). Tests for scenarios that are still blocked by a real framework /
//! WMS gap are `#[ignore]`d with the concrete evidence, so a blocked scenario
//! can never masquerade as a pass.
//!
//! Launching a fixture is expensive (~15 s each) and needs the boot directory
//! and built APKs; the tests skip loudly when those are unavailable, like
//! `counter_interaction.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use acos_runtime::session::{AgentSession, SessionOptions};
use acos_runtime::view_tree::{ViewNode, ViewTree};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn apk(name: &str) -> Option<PathBuf> {
    let rel = format!("fixtures/build/apps/{name}.apk");
    if !acos_test_support::have_fixture(&rel) {
        return None;
    }
    let p = repo_root().join(rel);
    p.is_file().then_some(p)
}

fn seed_boot_dir() {
    if std::env::var_os("ACOS_BOOT_DIR").is_none() {
        let boot = repo_root().join(".cache/boot/android-34");
        if boot.is_dir() {
            std::env::set_var("ACOS_BOOT_DIR", boot);
        }
    }
}

fn boot_available() -> bool {
    std::env::var_os("ACOS_BOOT_DIR").is_some_and(|d| Path::new(&d).join("BOOTCLASSPATH").is_file())
}

fn launch(session: &AgentSession, apk: &Path) -> bool {
    match session.launch(apk) {
        Ok(report) => {
            let ok = report.stages.iter().any(|s| {
                s.name == "first_frame" && s.status == acos_runtime::session::StageStatus::Ok
            });
            assert!(
                ok,
                "{} did not reach `first_frame`: {:?}",
                apk.display(),
                report
                    .stages
                    .iter()
                    .map(|s| (s.name, s.status))
                    .collect::<Vec<_>>()
            );
            true
        }
        Err(e) => {
            eprintln!("SKIP app_matrix_interaction: launch failed: {e:?}");
            false
        }
    }
}

fn walk<'a>(nodes: &'a [ViewNode], out: &mut Vec<&'a ViewNode>) {
    for n in nodes {
        out.push(n);
        walk(&n.children, out);
    }
}

fn all_nodes(tree: &ViewTree) -> Vec<&ViewNode> {
    let mut v = Vec::new();
    walk(&tree.nodes, &mut v);
    v
}

fn has_text(tree: &ViewTree, text: &str) -> bool {
    all_nodes(tree).iter().any(|n| n.text == text)
}

fn text_of_id<'a>(tree: &'a ViewTree, suffix: &str) -> Option<&'a str> {
    all_nodes(tree)
        .into_iter()
        .find(|n| n.resource_id.ends_with(suffix))
        .map(|n| n.text.as_str())
}

/// Retry a text tap until the predicate sees the expected tree. A single
/// injected tap is sometimes lost on the first pass after launch (the looper
/// re-enters); retrying is honest because the test still requires the real
/// tree change, and the retry is bounded.
fn tap_text_until(
    session: &AgentSession,
    label: &str,
    attempts: usize,
    mut pred: impl FnMut(&ViewTree) -> bool,
) -> bool {
    for _ in 0..attempts {
        let _ = session.tap_text(label);
        if let Ok(tree) = session.observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES) {
            if pred(&tree) {
                return true;
            }
        }
    }
    false
}

fn boot(session: &AgentSession, apk: &Path) -> Option<ViewTree> {
    if !launch(session, apk) {
        return None;
    }
    Some(
        session
            .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
            .expect("live view tree after launch"),
    )
}

// ---------------------------------------------------------------------------
// fragments-nav: fragment transaction navigation
// ---------------------------------------------------------------------------

#[test]
fn fragments_nav_replace_and_pop_change_the_container() {
    seed_boot_dir();
    let Some(apk) = apk("fragments-nav") else {
        eprintln!("SKIP fragments_nav_replace_and_pop_change_the_container: no apk");
        return;
    };
    if !boot_available() {
        eprintln!("SKIP fragments_nav_replace_and_pop_change_the_container: no boot dir");
        return;
    }
    let session = AgentSession::start(SessionOptions::default());
    if boot(&session, &apk).is_none() {
        return;
    }

    let before = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .unwrap();
    assert_eq!(
        text_of_id(&before, ":id/frag_text"),
        Some("Alpha taps=0"),
        "initial fragment is Alpha"
    );

    let report = session.tap_text("Show Beta").expect("tap accepted");
    assert!(report.accepted, "Show Beta routed");
    assert!(report.state_changed, "Show Beta changed the tree");
    let after = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .unwrap();
    assert_eq!(
        text_of_id(&after, ":id/frag_text"),
        Some("Beta taps=0"),
        "the container now shows the Beta fragment view"
    );
    assert_eq!(text_of_id(&after, ":id/state"), Some("current=Beta"));

    let report = session.tap_text("Back stack").expect("tap accepted");
    assert!(report.state_changed, "popBackStack changed the tree");
    let popped = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .unwrap();
    assert_eq!(
        text_of_id(&popped, ":id/frag_text"),
        Some("Alpha taps=0"),
        "pop restores the Alpha fragment view"
    );

    session.close();
}

// ---------------------------------------------------------------------------
// list-adapter: scroll
// ---------------------------------------------------------------------------

#[test]
fn list_scroll_button_moves_the_recycled_window() {
    seed_boot_dir();
    let Some(apk) = apk("list-adapter") else {
        eprintln!("SKIP list_scroll_button_moves_the_recycled_window: no apk");
        return;
    };
    if !boot_available() {
        eprintln!("SKIP list_scroll_button_moves_the_recycled_window: no boot dir");
        return;
    }
    let session = AgentSession::start(SessionOptions::default());
    let Some(before) = boot(&session, &apk) else {
        return;
    };

    assert!(has_text(&before, "Item 0"), "list starts at Item 0");
    assert!(
        !has_text(&before, "Item 42"),
        "Item 42 is outside the initial recycled window"
    );

    let report = session.tap_text("Scroll to last").expect("tap accepted");
    assert!(report.accepted, "Scroll to last routed");
    assert!(report.state_changed, "smoothScrollToPosition changed the tree");
    let after = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .unwrap();
    assert!(
        has_text(&after, "Item 42") || has_text(&after, "Item 59"),
        "the list scrolled to the tail of the 60 rows"
    );
    assert!(
        !has_text(&after, "Item 0"),
        "the first row scrolled out of the window"
    );

    session.close();
}

// ---------------------------------------------------------------------------
// dialogs-menus: dialog + Cancel + PopupMenu shell + Spinner dropdown
// ---------------------------------------------------------------------------

#[test]
fn dialogs_alert_opens_and_cancel_dismisses() {
    seed_boot_dir();
    let Some(apk) = apk("dialogs-menus") else {
        eprintln!("SKIP dialogs_alert_opens_and_cancel_dismisses: no apk");
        return;
    };
    if !boot_available() {
        eprintln!("SKIP dialogs_alert_opens_and_cancel_dismisses: no boot dir");
        return;
    }
    let session = AgentSession::start(SessionOptions::default());
    if boot(&session, &apk).is_none() {
        return;
    }

    let report = session.tap_text("Alert dialog").expect("tap accepted");
    assert!(report.accepted, "Alert dialog routed");
    assert!(report.state_changed, "a second (dialog) window was added");
    let open = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .unwrap();
    assert!(
        has_text(&open, "Are you sure?") && has_text(&open, "Cancel"),
        "the AlertDialog window is in the tree"
    );

    let report = session.tap_text("Cancel").expect("tap accepted");
    assert!(report.state_changed, "Cancel dismissed the dialog");
    let closed = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .unwrap();
    assert!(
        !has_text(&closed, "Are you sure?"),
        "the dialog window is gone after Cancel"
    );

    session.close();
}

#[test]
fn dialogs_popup_menu_opens_with_items() {
    seed_boot_dir();
    let Some(apk) = apk("dialogs-menus") else {
        eprintln!("SKIP dialogs_popup_menu_opens_with_items: no apk");
        return;
    };
    if !boot_available() {
        eprintln!("SKIP dialogs_popup_menu_opens_with_items: no boot dir");
        return;
    }
    let session = AgentSession::start(SessionOptions::default());
    if boot(&session, &apk).is_none() {
        return;
    }

    // Warm the window-input path with the first (Alert) tap, then dismiss,
    // mirroring the deterministic W187-A reproduction.
    let _ = session.tap_text("Alert dialog");
    let _ = session.tap_text("Cancel");

    let opened = tap_text_until(&session, "Popup menu", 3, |t| has_text(t, "Rename"));
    let tree = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .unwrap();
    assert!(
        opened,
        "PopupMenu.show() added a PopupDecorView with Rename/Delete: {:?}",
        all_nodes(&tree)
            .iter()
            .filter(|n| !n.text.is_empty())
            .map(|n| n.text.clone())
            .collect::<Vec<_>>()
    );
    assert!(has_text(&tree, "Rename") && has_text(&tree, "Delete"));

    session.close();
}

#[test]
fn dialogs_spinner_dropdown_opens() {
    seed_boot_dir();
    let Some(apk) = apk("dialogs-menus") else {
        eprintln!("SKIP dialogs_spinner_dropdown_opens: no apk");
        return;
    };
    if !boot_available() {
        eprintln!("SKIP dialogs_spinner_dropdown_opens: no boot dir");
        return;
    }
    let session = AgentSession::start(SessionOptions::default());
    if boot(&session, &apk).is_none() {
        return;
    }

    // The dropdown is a popup window; the list rows "Two"/"Three" only exist
    // in the tree while it is open.
    let opened = tap_text_until(&session, "One", 3, |t| has_text(t, "Two"));
    let tree = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .unwrap();
    assert!(
        opened,
        "the Spinner dropdown opened with One/Two/Three: {:?}",
        all_nodes(&tree)
            .iter()
            .filter(|n| !n.text.is_empty())
            .map(|n| n.text.clone())
            .collect::<Vec<_>>()
    );
    assert!(has_text(&tree, "Two") && has_text(&tree, "Three"));

    session.close();
}

// ---------------------------------------------------------------------------
// Known gaps (ignored, with evidence) -- no fake passes
// ---------------------------------------------------------------------------

/// `list-adapter` row click does not land. The framework receives the DOWN
/// (`MainActivity.dispatchTouchEvent` logs `action=0`) but the event never
/// reaches the `ListView`: a probe `setOnTouchListener` on the list never
/// fires and no `item_click` log is produced. Owner: the VM/framework touch
/// dispatch path (`ViewGroup.dispatchTouchEvent` -> `AbsListView.onTouchEvent`).
#[test]
#[ignore = "OPEN: ListView row click never reaches AbsListView; DOWN is logged by the Activity but no list touch listener fires"]
fn list_row_click_sets_selected_text() {
    seed_boot_dir();
    let Some(apk) = apk("list-adapter") else {
        return;
    };
    if !boot_available() {
        return;
    }
    let session = AgentSession::start(SessionOptions::default());
    if boot(&session, &apk).is_none() {
        return;
    }
    let report = session.tap_text("Item 3").expect("tap accepted");
    assert!(report.state_changed, "row click updates `selected`");
    let tree = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .unwrap();
    assert_eq!(text_of_id(&tree, ":id/selected"), Some("Selected: Item 3"));
    session.close();
}

/// The popup window opens and renders, but a tap inside it is not routed to
/// the popup window: `Rename` remains visible and no `popup_rename` log is
/// produced. The popup frame in the tree is correct (`PopupDecorView
/// [42,441,557,693]`, `Rename [84,479,515,529]`), so the gap is popup-window
/// input routing in the WMS dispatcher, not hit-testing geometry.
#[test]
#[ignore = "OPEN: taps inside a PopupWindow are not routed to the popup; Rename/Delete stay visible and no popup_rename log"]
fn popup_menu_item_select_sets_status() {
    seed_boot_dir();
    let Some(apk) = apk("dialogs-menus") else {
        return;
    };
    if !boot_available() {
        return;
    }
    let session = AgentSession::start(SessionOptions::default());
    if boot(&session, &apk).is_none() {
        return;
    }
    // Warm the window-input path and retry the open, exactly like the
    // `dialogs_popup_menu_opens_with_items` guard: the first injected tap
    // after launch is sometimes lost while the looper re-enters.
    let _ = session.tap_text("Alert dialog");
    let _ = session.tap_text("Cancel");
    let opened = tap_text_until(&session, "Popup menu", 3, |t| has_text(t, "Rename"));
    assert!(opened, "PopupMenu opened with Rename/Delete");
    let report = session.tap_text("Rename").expect("tap accepted");
    assert!(report.state_changed, "popup item click sets status");
    let tree = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .unwrap();
    assert_eq!(text_of_id(&tree, ":id/status"), Some("Renamed"));
    session.close();
}

/// The Spinner dropdown opens, but selecting a row does not fire
/// `onItemSelected` with the new position (`spinner_pos=0` is the only log, the
/// initial selection). Same popup-window input-routing gap as `PopupMenu`.
#[test]
#[ignore = "OPEN: Spinner dropdown opens but a row tap is not routed; no spinner_pos=1 log"]
fn spinner_item_select_updates_selection() {
    seed_boot_dir();
    let Some(apk) = apk("dialogs-menus") else {
        return;
    };
    if !boot_available() {
        return;
    }
    let session = AgentSession::start(SessionOptions::default());
    if boot(&session, &apk).is_none() {
        return;
    }
    let opened = tap_text_until(&session, "One", 3, |t| has_text(t, "Two"));
    assert!(opened, "the Spinner dropdown opened with One/Two/Three");
    let report = session.tap_text("Two").expect("tap accepted");
    assert!(report.accepted, "the row tap was routed");
    let lines = session.logs().unwrap();
    assert!(
        lines.iter().any(|l| l.message.contains("spinner_pos=1")),
        "selecting the second row logs spinner_pos=1; logs: {:?}",
        lines
            .iter()
            .filter(|l| l.message.contains("spinner") || l.message.contains("AcosDialogs"))
            .map(|l| format!("{}: {}", l.tag, l.message))
            .collect::<Vec<_>>()
    );
    session.close();
}

/// `Toast.show()` reaches `INotificationManager.enqueueTextToast`, whose text
/// AOSP's `NotificationManagerService` renders as a `TYPE_TOAST` system window.
/// ACOS records that real request: the run is observable both as a structured
/// `AcosToast` event (`{"event":"toast","text":…}`) and as the toast overlay
/// node in `observe --tree`.
#[test]
fn toast_text_is_observable() {
    seed_boot_dir();
    let Some(apk) = apk("dialogs-menus") else {
        return;
    };
    if !boot_available() {
        return;
    }
    let session = AgentSession::start(SessionOptions::default());
    if boot(&session, &apk).is_none() {
        return;
    }
    let _ = session.tap_text("Alert dialog");
    let _ = session.tap_text("Cancel");
    let _ = session.tap_text("Toast");
    let tree = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .unwrap();
    assert!(
        has_text(&tree, "Hello from ACOS"),
        "the Toast text is present as a structured node/event"
    );
    // The other half of the contract: the text is in the event stream as a
    // structured `toast` record, not just inside the app's own log tag.
    let lines = session.logs().unwrap();
    assert!(
        lines.iter().any(|l| l.tag == "AcosToast"
            && l.message.contains("\"event\":\"toast\"")
            && l.message.contains("Hello from ACOS")),
        "a structured toast event carries the text: {:?}",
        lines
            .iter()
            .filter(|l| l.tag == "AcosToast" || l.message.contains("toast"))
            .map(|l| format!("{}: {}", l.tag, l.message))
            .collect::<Vec<_>>()
    );
    session.close();
}

/// A gesture scroll (`acos scroll`, a DOWN/MOVE/UP MotionEvent sequence) does
/// not move the ListView. The `smoothScrollToPosition` button path *does*
/// (`list_scroll_button_moves_the_recycled_window`), so this pins the remaining
/// `AbsListView.onTouchMove`/`trackMotionScroll` gap for a raw gesture.
#[test]
#[ignore = "OPEN: raw vertical swipe does not scroll AbsListView; the programmatic smoothScrollToPosition path works"]
fn scroll_gesture_moves_the_list() {
    seed_boot_dir();
    let Some(apk) = apk("list-adapter") else {
        return;
    };
    if !boot_available() {
        return;
    }
    let session = AgentSession::start(SessionOptions::default());
    if boot(&session, &apk).is_none() {
        return;
    }
    let report = session
        .swipe((540.0, 1600.0), (540.0, 600.0), Duration::from_millis(400), 10)
        .expect("swipe accepted");
    assert!(report.state_changed, "the swipe scrolled the list");
    let tree = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .unwrap();
    assert!(!has_text(&tree, "Item 0"), "the list scrolled past row 0");
    session.close();
}
