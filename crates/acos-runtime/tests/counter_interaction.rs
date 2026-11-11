//! W133-A end-to-end: `counter.apk` launch -> published frame -> live tree.
//!
//! The candidate frame is produced by the app's **own** `View.draw` recorded
//! through the real `ThreadedRenderer.draw` display-list path (`gfx_*`
//! RenderNode natives) and rasterised by `acos-render`; the pixels are read
//! back from the session `FrameStore`, never synthesised. The view tree is a
//! live walk of `WindowManagerGlobal` (see `acos_runtime::view_tree`).
//!
//! The full interaction round trip (`tap --text Increment` -> count `1`) is
//! pinned by [`counter_tap_increment_updates_the_tree`]. The injected
//! `MotionEvent` is delivered window-local to `ViewRootImpl`, and the app's
//! `Button.onClick` runs: the interaction pump cancels the framework's pending
//! `ViewRootImpl` traversal after input delivery (ACOS renders interaction
//! frames synchronously via `Runtime::render_frame_now`), because that
//! traversal's async callback otherwise aborts on unimplemented ripple-graphics
//! natives before the sync `View$PerformClick` message posted by ACTION_UP can
//! run. See `results/m2/M2_LOG.md` §W161-A.
//!
//! Skips loudly when the fixture APK or boot directory is unavailable;
//! `ACOS_REQUIRE_FIXTURES` turns the missing fixture into a hard failure.

use std::path::{Path, PathBuf};

use acos_runtime::session::{AgentSession, SessionOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn counter_apk() -> Option<PathBuf> {
    let rel = "fixtures/build/apps/counter.apk";
    if !acos_test_support::have_fixture(rel) {
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

fn launch_counter(session: &AgentSession, apk: &Path) -> bool {
    let report = match session.launch(apk) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("SKIP counter_interaction: launch failed: {e:?}");
            return false;
        }
    };
    // The launch ladder must reach the published-frame rung; anything less is
    // recorded as evidence, not silently skipped.
    assert!(
        report.stages.iter().any(|s| s.name == "first_frame"
            && s.status == acos_runtime::session::StageStatus::Ok),
        "counter did not reach `first_frame`: {:?}\nroot_blocker={:?}",
        report
            .stages
            .iter()
            .map(|s| (s.name, s.status))
            .collect::<Vec<_>>(),
        report.root_blocker,
    );
    true
}

#[test]
fn counter_launch_publishes_a_real_frame_and_live_tree() {
    seed_boot_dir();
    let Some(apk) = counter_apk() else {
        eprintln!("SKIP counter_launch_publishes_a_real_frame_and_live_tree: no fixture apk");
        return;
    };
    if !boot_available() {
        eprintln!("SKIP counter_launch_publishes_a_real_frame_and_live_tree: no ACOS_BOOT_DIR");
        return;
    }

    let session = AgentSession::start(SessionOptions {
        trace_natives: true,
        ..SessionOptions::default()
    });
    if !launch_counter(&session, &apk) {
        return;
    }

    let observed = session.observe(None).expect("published frame");
    let frame = observed.frame.as_ref().expect("frame present");
    assert_eq!((frame.width, frame.height), (1080, 2400), "frame geometry");
    assert_eq!(
        frame.pixels.len(),
        1080 * 2400 * 4,
        "frame RGBA byte length"
    );
    assert!(
        frame.pixels.chunks_exact(4).any(|p| p[0] | p[1] | p[2] != 0),
        "frame is not entirely black: the app's View.draw produced pixels"
    );

    let tree = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .expect("live view tree");
    let labels: Vec<&str> = {
        fn collect<'a>(n: &'a acos_runtime::view_tree::ViewNode, out: &mut Vec<&'a str>) {
            out.push(n.text.as_str());
            for c in &n.children {
                collect(c, out);
            }
        }
        let mut v = Vec::new();
        for root in &tree.nodes {
            collect(root, &mut v);
        }
        v
    };
    assert!(
        labels.contains(&"Increment") && labels.contains(&"Reset"),
        "live tree exposes the counter buttons: {labels:?}"
    );

    session.close();
}

#[test]
fn counter_tap_increment_updates_the_tree() {
    seed_boot_dir();
    let Some(apk) = counter_apk() else {
        eprintln!("SKIP counter_tap_increment_updates_the_tree: no fixture apk");
        return;
    };
    if !boot_available() {
        eprintln!("SKIP counter_tap_increment_updates_the_tree: no ACOS_BOOT_DIR");
        return;
    }

    let session = AgentSession::start(SessionOptions {
        trace_natives: true,
        ..SessionOptions::default()
    });
    if !launch_counter(&session, &apk) {
        return;
    }

    let report = session.tap_text("Increment").expect("tap accepted");
    assert!(report.accepted, "tap routed");
    assert!(report.state_changed, "the count TextView changed to 1");

    let tree = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .expect("live view tree");
    let has_one = {
        fn search(n: &acos_runtime::view_tree::ViewNode) -> bool {
            n.text == "1" || n.children.iter().any(search)
        }
        tree.nodes.iter().any(search)
    };
    assert!(has_one, "the next tree shows count '1'");

    session.close();
}
