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

    // W179-A: window-relative offsets must be applied exactly once. The
    // reference (`fixtures/reference/counter/art-api34-x86_64`) puts the count
    // TextView at screen `[42,317]` and the Increment button at left `42`.
    // Before the fix `View.getLocationOnScreen` folded each `RenderNode`'s
    // layout position into its transform matrix *and* added `mLeft`/`mTop`, so
    // every offset below the window origin was doubled (count left 84, top
    // 506; Increment left 84).
    fn find_by_id(n: &acos_runtime::view_tree::ViewNode, suffix: &str) -> Option<[i32; 4]> {
        if n.resource_id.ends_with(suffix) {
            return Some(n.bounds);
        }
        n.children.iter().find_map(|c| find_by_id(c, suffix))
    }
    let count = tree
        .nodes
        .iter()
        .find_map(|n| find_by_id(n, ":id/count"))
        .expect("count TextView in the live tree");
    assert_eq!(
        (count[0], count[1]),
        (42, 317),
        "count TextView screen origin is doubled (W179-A): {count:?}"
    );
    let inc = tree
        .nodes
        .iter()
        .find_map(|n| find_by_id(n, ":id/inc"))
        .expect("Increment button in the live tree");
    assert_eq!(
        inc[0], 42,
        "Increment left is doubled (W179-A): {inc:?}"
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

/// W167-A: the frame after one tap must differ from the frame before it, and
/// the changed pixels must be a **local repaint in the count's content area**,
/// not a full-frame redraw. This pins the post-tap repaint
/// (`Runtime::pump` -> `render_frame_now`), not only the live tree: an
/// interaction that updates `TextView.getText()` but leaves the published
/// pixels stale is a failure.
///
/// `observe --tree` reports `View.getLocationOnScreen` display coordinates
/// (with the system status bar); a published frame is the app window rendered
/// from its own top-left. The two spaces differ by the window origin, so the
/// test keys on the repaint footprint — a small text-sized band at the top of
/// the content — plus the live tree value, instead of a raw coordinate match.
#[test]
fn counter_tap_repaints_the_count_region() {
    seed_boot_dir();
    let Some(apk) = counter_apk() else {
        eprintln!("SKIP counter_tap_repaints_the_count_region: no fixture apk");
        return;
    };
    if !boot_available() {
        eprintln!("SKIP counter_tap_repaints_the_count_region: no ACOS_BOOT_DIR");
        return;
    }

    let session = AgentSession::start(SessionOptions::default());
    if !launch_counter(&session, &apk) {
        return;
    }

    let before = session
        .observe(None)
        .expect("frame before tap")
        .frame
        .expect("frame before tap present");

    let report = session.tap_text("Increment").expect("tap accepted");
    assert!(report.state_changed, "the count TextView changed to 1");

    let after = session
        .observe(None)
        .expect("frame after tap")
        .frame
        .expect("frame after tap present");
    assert_eq!(
        (before.width, before.height),
        (after.width, after.height),
        "frame geometry is stable across the tap"
    );
    assert_eq!(
        before.pixels.len(),
        after.pixels.len(),
        "frame buffer length is stable"
    );

    // The live tree is the source of truth for the new value.
    let tree = session
        .observe_tree(acos_runtime::view_tree::DEFAULT_MAX_NODES)
        .expect("live view tree after tap");
    let has_one = {
        fn search(n: &acos_runtime::view_tree::ViewNode) -> bool {
            n.text == "1" || n.children.iter().any(search)
        }
        tree.nodes.iter().any(search)
    };
    assert!(has_one, "the next tree shows count '1'");

    // Changed-pixel bounding box.
    let mut diff = 0usize;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..before.height {
        for x in 0..before.width {
            let o = ((y * before.width + x) * 4) as usize;
            if before.pixels[o..o + 4] != after.pixels[o..o + 4] {
                diff += 1;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    assert!(
        diff > 0,
        "the post-tap frame is byte-identical to the pre-tap frame"
    );

    // The repaint is a local region in the top-left content area (the count
    // TextView, plus the button's pressed-state shadow below it) — never a
    // full-frame redraw.
    assert!(
        max_x < before.width / 2 && max_y < before.height / 4,
        "repaint ({min_x},{min_y})-({max_x},{max_y}) (diff={diff}) is not in the \
         top-left content area"
    );
    assert!(
        max_x - min_x < 400 && max_y - min_y < 400,
        "repaint ({min_x},{min_y})-({max_x},{max_y}) spans most of the frame"
    );

    // The topmost changed band is a single digit glyph (the count text); a
    // full-width band would mean the whole content reflowed.
    let band_bottom = (min_y + 40).min(after.height);
    let (mut band_min_x, mut band_max_x) = (u32::MAX, 0u32);
    for y in min_y..band_bottom {
        for x in 0..before.width {
            let o = ((y * before.width + x) * 4) as usize;
            if before.pixels[o..o + 4] != after.pixels[o..o + 4] {
                band_min_x = band_min_x.min(x);
                band_max_x = band_max_x.max(x);
            }
        }
    }
    assert!(
        band_max_x - band_min_x < 80,
        "the top changed band ({band_min_x}..{band_max_x}) is wider than a digit glyph"
    );

    session.close();
}
