//! WV8 acceptance: the `useWideViewPort` / `loadWithOverviewMode` arms, against
//! the **real** pinned engine over the real wire.
//!
//! The contract (recorded in `DECISIONS.md`, ADR-0031 note):
//!
//! * `useWideViewPort = false` (**default**) — the layout viewport is the View's
//!   width in CSS px (`device px / density`) and `<meta name="viewport">` is
//!   ignored. That is what ACOS's WebView has always done and what Chromium's
//!   own `WebView` does with the setting off.
//! * `useWideViewPort = true` — the page's `<meta viewport>` is honoured; without
//!   one the layout is Chromium's Android default of 980 CSS px.
//! * `loadWithOverviewMode = true` — the page is scaled down so its whole width
//!   fits the View; the layout width does not change.
//!
//! Both fixture pages exist: one with `<meta name="viewport" content="width=640">`
//! and one without (a `data:` URL). The assertions read the page's own
//! `document.documentElement.clientWidth` and `visualViewport.scale` through
//! `view.evaluateJavascript`, i.e. what the framework would see, not a number
//! this test computed.
//!
//! Heavy (it starts Chromium), so it is guarded and opt-in:
//!
//! ```text
//! tools/art-runtime/run-exclusive.sh --task WV8 -- tools/guard/guarded.sh --mem 8G --time 420 -- \
//!   env ACOS_WEBVIEW_TEST=1 CARGO_TARGET_DIR=$PWD/.cache/wv8-target \
//!   cargo test -p acos-webview --test wv8_viewport -- --nocapture --test-threads=1
//! ```

mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use acos_webview::gate::{BrokerSource, Gate, GateConfig};
use common::WireGuest;
use serde_json::json;

const META_PAGE: &str = "<!doctype html><html><head><meta charset='utf-8'>\
     <meta name='viewport' content='width=640'><title>meta</title></head>\
     <body style='font:18px sans-serif'><h1 id=h>meta page</h1>\
     <div id=out>meta viewport page body</div></body></html>";

const NO_META_PAGE: &str = "<!doctype html><html><head><meta charset='utf-8'>\
     <title>plain</title></head><body style='font:18px sans-serif'><h1 id=h>plain page</h1>\
     <div id=out>no meta viewport</div></body></html>";

fn engine_test_enabled() -> bool {
    std::env::var("ACOS_WEBVIEW_TEST").as_deref() == Ok("1")
}

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("ACOS_VIEW_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("acos-wv8-{}", std::process::id())));
    let dir = base.join(format!("e{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn gate(dir: &Path, broker_socket: &Path, bin: &Path) -> Gate {
    Gate::start(GateConfig {
        session_dir: dir.join("s"),
        session_id: "wv8-viewport".to_string(),
        broker_socket: broker_socket.to_path_buf(),
        broker: BrokerSource::Binary(bin.to_path_buf()),
        engine_cache: std::env::var_os("ACOS_WEBVIEW_CACHE").map(PathBuf::from),
        idle_shutdown: Some(Duration::from_secs(10)),
        log: Some(dir.join("broker.log")),
        spawn_wait: acos_webview::gate::SPAWN_WAIT,
    })
    .expect("start gate")
}

/// `evaluateJavascript` answering a JSON string: the value the page produced.
///
/// Observed while measuring this arm: for a few milliseconds after a
/// `setDeviceMetricsOverride` (`useWideViewPort` re-sends the metrics) the
/// target's `document.documentElement` can be absent — Chromium is between
/// commits — and an unguarded `document.documentElement.clientWidth` throws.
/// The retry is bounded and only for that window; every assertion still reads
/// the page's own value, and a page that never commits still fails the test.
fn eval(guest: &WireGuest, view: i64, script: &str) -> String {
    let mut last = None;
    let reply = loop {
        match guest.call(
            "view.evaluateJavascript",
            json!({"view": view, "script": script}),
        ) {
            Ok(reply) => break reply,
            Err(error) => {
                let text = error.to_string();
                if !text.contains("Cannot read properties of null") || last.is_some() {
                    panic!("evaluate {script}: {error}");
                }
                last = Some(error);
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let value = reply["valueJson"].as_str().unwrap_or_default().to_string();
    // The wire carries the JSON encoding of the JS result ("\"640\"").
    serde_json::from_str::<String>(&value).unwrap_or(value)
}

fn layout_width(guest: &WireGuest, view: i64) -> u32 {
    eval(guest, view, "String(document.documentElement.clientWidth)")
        .parse()
        .unwrap_or_else(|_| panic!("clientWidth is not a number for view {view}"))
}

fn page_scale(guest: &WireGuest, view: i64) -> f64 {
    eval(
        guest,
        view,
        "(window.visualViewport ? window.visualViewport.scale : 1).toFixed(3)",
    )
    .parse()
    .unwrap_or_else(|_| panic!("visualViewport.scale is not a number for view {view}"))
}

fn load(guest: &WireGuest, view: i64, data: &str) {
    guest
        .call(
            "view.loadData",
            json!({"view": view, "mime": "text/html", "encoding": "utf8", "data": data}),
        )
        .unwrap_or_else(|error| panic!("view.loadData for {view}: {error}"));
}

#[test]
fn the_default_arm_lays_out_at_the_view_width_and_ignores_meta_viewport() {
    if acos_test_support::require_prereq(
        engine_test_enabled(),
        "the_default_arm_lays_out_at_the_view_width_and_ignores_meta_viewport",
        "set ACOS_WEBVIEW_TEST=1 to run this heavy Chromium test (the release gate runs it with the env set; see tools/verify/test-prereq-optouts.txt)",
    )
    .is_none()
    {
        return;
    }
    let root = scratch("default");
    let broker_socket = root.join("run").join("broker.sock");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_acos-webview"));
    let g = gate(&root, &broker_socket, &bin);
    let guest = WireGuest::connect(g.socket());
    guest.hello();
    // 1080 device px at 2.625 = 411 CSS px, the geometry the ART scenario uses.
    guest
        .call(
            "view.create",
            json!({"view": 1, "width": 1080, "height": 2400, "density": 2.625}),
        )
        .expect("view.create");

    // No `<meta viewport>`: the layout viewport is the View's CSS width.
    load(&guest, 1, NO_META_PAGE);
    assert_eq!(
        layout_width(&guest, 1),
        411,
        "the default arm lays a page without <meta viewport> out at the View's CSS width"
    );
    assert_eq!(page_scale(&guest, 1), 1.0, "no overview mode: 100 % scale");

    // With `<meta viewport content='width=640'>` the meta tag is **ignored**: the
    // default arm is Chromium's `mobile: false` layout.
    load(&guest, 1, META_PAGE);
    assert_eq!(
        layout_width(&guest, 1),
        411,
        "the default arm ignores <meta viewport> (that is `useWideViewPort(false)`)"
    );

    // The two switches are applied, never silently dropped: the reply names them
    // in `applied`, so the guest's setter does not throw.
    let reply = guest
        .call(
            "view.settings",
            json!({"view": 1, "settings": {"useWideViewPort": false, "loadWithOverviewMode": false}}),
        )
        .expect("view.settings");
    let applied: Vec<&str> = reply["applied"]
        .as_array()
        .expect("applied")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(applied.contains(&"useWideViewPort"), "{reply}");
    assert!(applied.contains(&"loadWithOverviewMode"), "{reply}");
    assert_eq!(reply["rejected"], json!([]));

    guest.close();
    drop(g);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_wide_arm_honours_meta_viewport_and_the_980_default_and_overview_fits_it() {
    if acos_test_support::require_prereq(
        engine_test_enabled(),
        "the_wide_arm_honours_meta_viewport_and_the_980_default_and_overview_fits_it",
        "set ACOS_WEBVIEW_TEST=1 to run this heavy Chromium test (the release gate runs it with the env set; see tools/verify/test-prereq-optouts.txt)",
    )
    .is_none()
    {
        return;
    }
    let root = scratch("wide");
    let broker_socket = root.join("run").join("broker.sock");
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_acos-webview"));
    let g = gate(&root, &broker_socket, &bin);
    let guest = WireGuest::connect(g.socket());
    guest.hello();
    guest
        .call(
            "view.create",
            json!({"view": 2, "width": 1080, "height": 2400, "density": 2.625}),
        )
        .expect("view.create");

    // The wide arm, applied the way the guest applies it: before the first load.
    let reply = guest
        .call(
            "view.settings",
            json!({"view": 2, "settings": {"useWideViewPort": true}}),
        )
        .expect("view.settings wide");
    assert_eq!(reply["rejected"], json!([]), "{reply}");

    // `<meta viewport content='width=640'>` is honoured.
    load(&guest, 2, META_PAGE);
    assert_eq!(
        layout_width(&guest, 2),
        640,
        "the wide arm honours <meta viewport>"
    );

    // Without a meta tag, Chromium's Android default layout viewport is 980 CSS px.
    load(&guest, 2, NO_META_PAGE);
    assert_eq!(
        layout_width(&guest, 2),
        980,
        "the wide arm lays a page without <meta viewport> out at 980 CSS px"
    );

    // `loadWithOverviewMode`: the whole page width fits the View — a visual
    // scale of 411/980 — and the layout width is unchanged.
    let reply = guest
        .call(
            "view.settings",
            json!({"view": 2, "settings": {"loadWithOverviewMode": true}}),
        )
        .expect("view.settings overview");
    assert_eq!(reply["rejected"], json!([]), "{reply}");
    let scale = page_scale(&guest, 2);
    assert!(
        (scale - 411.0 / 980.0).abs() < 0.03,
        "overview mode must fit 411 CSS px of View over a 980 CSS px page (411/980 = {}), got {scale}",
        411.0 / 980.0
    );
    assert_eq!(
        layout_width(&guest, 2),
        980,
        "overview mode scales; it does not change the layout width"
    );

    // A *new* document under overview mode lays out at its own width, not at
    // `viewport / scale`: a page scale left over from the previous document would
    // otherwise become the next document's initial scale (measured on the ART
    // arm: the wide 980 px page came out at 343 with the old 0.35 scale still in
    // force). The broker resets the scale before a navigation and re-fits after.
    load(&guest, 2, META_PAGE);
    assert_eq!(
        layout_width(&guest, 2),
        640,
        "a meta-viewport page loaded under overview mode keeps its layout width"
    );
    let scale = page_scale(&guest, 2);
    assert!(
        (scale - 411.0 / 640.0).abs() < 0.03,
        "and is fitted to the View after the load, got {scale}"
    );
    load(&guest, 2, NO_META_PAGE);
    assert_eq!(
        layout_width(&guest, 2),
        980,
        "a page without a meta tag loaded under overview mode keeps the wide 980 px layout"
    );
    let scale = page_scale(&guest, 2);
    assert!(
        (scale - 411.0 / 980.0).abs() < 0.03,
        "and is fitted to the View, got {scale}"
    );

    // Back to the default arm: the layout returns to the View's width and the
    // scale to 100 %.
    guest
        .call(
            "view.settings",
            json!({"view": 2, "settings": {"useWideViewPort": false, "loadWithOverviewMode": false}}),
        )
        .expect("view.settings back to narrow");
    load(&guest, 2, NO_META_PAGE);
    assert_eq!(
        layout_width(&guest, 2),
        411,
        "narrow again after the switches"
    );
    assert_eq!(page_scale(&guest, 2), 1.0, "100 % again after the switches");

    guest.close();
    drop(g);
    let _ = std::fs::remove_dir_all(&root);
}
