//! WVFAIR: frame pulls are **fair** across views and sessions.
//!
//! One slow page used to starve every other WebView: `beginFrame` was
//! serialised by one mutex held for up to three 2 s attempts, so a guest pull —
//! and the frames `input.tap` needs — waited behind it (WV8 §5 measured a 2.0 s
//! tap ack and a lost click with two loading targets). This test starts the
//! **real** broker and the **real** pinned engine, creates two views with the
//! same page geometry (`900x560 @2.625`, the probe's), makes one of them
//! deliberately expensive to paint (a full-viewport canvas repainted every
//! animation frame, with shadowed rects, and a main thread that is never idle),
//! pulls frames from both as fast as the guest allows, and asserts:
//!
//! * the fast view's pulls stay bounded (`max`, `p95` and `median` budgets)
//!   while the slow view is pulling;
//! * a tap on the fast view during that window is acknowledged inside its
//!   budget **and reaches the page** (the page's own JS changed `#status`);
//! * both views still get real PNG frames (no invented pixels, no dropped
//!   pages).
//!
//! Both arrangements the brief names are measured: two views in **different
//! sessions** on one broker (each session attaches to the host-wide socket, the
//! way `gate` does) and two views in the **same session**.
//!
//! Guard requirements (it starts a Chromium):
//!
//! ```text
//! tools/art-runtime/run-exclusive.sh --task WVFAIR -- \
//!   tools/guarded.sh --mem 8G --time 600 -- \
//!   env ACOS_WEBVIEW_TEST=1 ACOS_WEBVIEW_CACHE=/home/Qui/Projects/acos/.cache/webview \
//!       ACOS_VIEW_TEST_DIR=$PWD/.cache/lv CARGO_TARGET_DIR=$PWD/.cache/wvf-target \
//!   cargo test -p acos-webview --test fair_pulls -- --nocapture --test-threads=1
//! ```
//!
//! Without `ACOS_WEBVIEW_TEST=1` it prints a loud SKIP and returns: it never
//! reports a pass it did not measure.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use acos_webview::broker::{Broker, BrokerConfig};
use acos_webview::engine::EngineCache;
use acos_webview::server::{serve_shared, SharedConfig};
use acos_webview::view::Viewport;
use common::{assert_png, WireGuest};
use serde_json::json;

/// The probe's View geometry (`scenarios/webview-probe.yaml`): 900x560 device
/// pixels on a 420 dpi display.
const WIDTH: u32 = 900;
const HEIGHT: u32 = 560;
const DENSITY: f64 = 2.625;

/// A cheap page with one real button: its click handler changes `#status`, so a
/// tap that never reached the page is visible as `#status` staying `idle`.
const FAST_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<title>fast</title></head><body style="margin:0;font:16px sans-serif;background:#fff">
<h1 id="h">fast page</h1>
<button id="b" style="display:block;width:240px;height:80px;font-size:20px"
  onclick="document.getElementById('status').textContent='tapped'">Tap me</button>
<div id="status" style="height:40px">idle</div>
</body></html>"#;

/// The page that becomes expensive on demand.
///
/// It paints one frame normally (so the view owns a real frame, exactly like a
/// page that loads fine and then runs heavy script), and `startSlowLoop()` — the
/// test's own `evaluateJavascript` — turns it into the case that starved the
/// other view: every animation frame repaints 600 shadowed rects into a
/// full-viewport canvas, and the main thread is kept busy in 1900 ms slices, so
/// a compositor frame cannot commit for ~2 s while the rest of the browser is
/// healthy.
const SLOW_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8">
<title>slow</title></head><body style="margin:0;background:#111">
<canvas id="c" width="900" height="560" style="filter: blur(3px)"></canvas>
<script>
let running = false;
function startSlowLoop(busyMs) {
  if (running) return 'already';
  running = true;
  busyMs = busyMs || 1900;
  const g = document.getElementById('c').getContext('2d');
  let n = 0;
  const paint = () => {
    for (let i = 0; i < 600; i++) {
      g.shadowBlur = 40;
      g.shadowColor = 'rgba(255,0,0,0.7)';
      g.fillStyle = 'hsl(' + ((n + i) * 7 % 360) + ',85%,55%)';
      g.fillRect((i * 37 + n * 3) % 900, (i * 23) % 560, 300, 220);
    }
    n++;
    requestAnimationFrame(paint);
  };
  paint();
  setInterval(() => { const t = performance.now(); while (performance.now() - t < busyMs) {} }, 0);
  return 'started';
}
window.startSlowLoop = startSlowLoop;
</script></body></html>"#;

/// The script that turns the slow page slow (called after its first frame).
const START_SLOW: &str = "startSlowLoop()";

/// The worker-pool arm's variant: a 900 ms busy slice instead of 1900. Its
/// point is the connection's shared worker pool (four frame pulls must not
/// occupy it), not engine stress; four 1.9 s pages plus their retries was
/// measured to crash the pinned engine once, and a fairness test must not
/// require a crash-tolerant engine.
const START_SLOW_POOL: &str = "startSlowLoop(900)";

fn engine_test_enabled() -> bool {
    std::env::var("ACOS_WEBVIEW_TEST").as_deref() == Ok("1")
}

/// `ACOS_WEBVIEW_FAIR_ARM=<tag>` runs only that arm — the evidence tool for a
/// before/after comparison (each arm is asserted either way; the selector never
/// turns an assertion off).
fn arm_selected(tag: &str) -> bool {
    match std::env::var("ACOS_WEBVIEW_FAIR_ARM") {
        Ok(wanted) => wanted.split(',').any(|w| w.trim() == tag),
        Err(_) => true,
    }
}

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::var_os("ACOS_VIEW_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("acos-wvfair-{}", std::process::id()))
        });
    let dir = base.join(format!("fair-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn base64(text: &str) -> String {
    acos_webview::view::base64_encode(text.as_bytes())
}

fn percentile(sorted: &[u128], p: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Create a view, load `page` and wait until it has painted once.
fn open_view(guest: &WireGuest, id: i64, page: &str, what: &str) -> i64 {
    guest
        .call(
            "view.create",
            json!({"view": id, "width": WIDTH, "height": HEIGHT, "density": DENSITY}),
        )
        .unwrap_or_else(|e| panic!("{what}: view.create failed: {e}"));
    guest
        .call(
            "view.loadData",
            json!({"view": id, "data": base64(page), "mime": "text/html", "encoding": "base64"}),
        )
        .unwrap_or_else(|e| panic!("{what}: loadData failed: {e}"));
    // The first frame is the load's own paint; a page that never paints would
    // fail here instead of silently measuring nothing.
    let (meta, blob) = guest.render(id);
    assert_png(&blob.data);
    // Frames stay at the engine's CSS resolution (ADR-0031 / WV7's measured
    // capture facts): the View's 900 device px at 2.625 are ~343 CSS px, and
    // the wire reports the frame's *real* size.
    let w = meta["width"].as_u64().unwrap_or(0);
    let h = meta["height"].as_u64().unwrap_or(0);
    assert!(
        w > 300 && w <= WIDTH as u64 && h > 180 && h <= HEIGHT as u64,
        "{what}: frame {w}x{h} is not a plausible CSS-resolution frame for a \
         {WIDTH}x{HEIGHT} @{DENSITY} View"
    );
    assert_eq!(blob.width as u64, w);
    assert_eq!(blob.height as u64, h);
    id
}

/// Turn the slow page's heavy loop on, after its first frame exists (the same
/// order a real page follows: it loads, then its script starts to hurt).
fn start_slow_loop(guest: &WireGuest, view: i64) {
    start_slow_loop_with(guest, view, START_SLOW)
}

fn start_slow_loop_with(guest: &WireGuest, view: i64, script: &str) {
    let r = guest
        .call(
            "view.evaluateJavascript",
            json!({"view": view, "script": script}),
        )
        .unwrap_or_else(|e| panic!("slow page loop: {e}"));
    assert!(
        r["valueJson"].as_str().unwrap_or("").contains("started"),
        "slow page loop did not start: {}",
        r["valueJson"]
    );
}

struct PullSeries {
    label: String,
    elapsed: Vec<u128>,
}

impl PullSeries {
    fn report(&self) -> serde_json::Value {
        let mut sorted = self.elapsed.clone();
        sorted.sort_unstable();
        json!({
            "label": self.label,
            "pulls": sorted.len(),
            "medianMs": percentile(&sorted, 0.50),
            "p95Ms": percentile(&sorted, 0.95),
            "maxMs": sorted.last().copied().unwrap_or(0),
        })
    }
}

/// Pull frames on `view` until `stop` is set, timing every pull.
fn pull_loop(guest: Arc<WireGuest>, view: i64, stop: Arc<AtomicBool>, label: &str) -> PullSeries {
    let mut elapsed = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        let t0 = Instant::now();
        let (meta, blob) = guest.render(view);
        elapsed.push(t0.elapsed().as_millis());
        assert_png(&blob.data);
        assert_eq!(meta["view"].as_i64(), Some(view));
    }
    PullSeries {
        label: label.to_string(),
        elapsed,
    }
}

/// Tap `#b` on `view` by the button's own geometry and wait for the page's JS.
fn tap_and_verify(guest: &WireGuest, view: i64) -> (u128, String) {
    let rect = guest
        .call(
            "view.evaluateJavascript",
            json!({
                "view": view,
                "script": "(() => { const r = document.getElementById('b').getBoundingClientRect(); \
                           return JSON.stringify({x: r.left + r.width / 2, y: r.top + r.height / 2}); })()",
            }),
        )
        .expect("button rect");
    let value = rect["valueJson"]
        .as_str()
        .unwrap_or("null")
        .trim_matches('"')
        .replace("\\\"", "\"");
    let point: serde_json::Value = serde_json::from_str(&value).expect("button rect json");
    let x = point["x"].as_f64().unwrap_or(0.0) * DENSITY;
    let y = point["y"].as_f64().unwrap_or(0.0) * DENSITY;
    let t0 = Instant::now();
    guest
        .call("input.tap", json!({"view": view, "x": x, "y": y}))
        .expect("input.tap");
    let ack = t0.elapsed().as_millis();
    // The click's own JS runs after the ack; poll for it briefly so a tap the
    // engine dropped cannot pass as one that landed.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut status = String::new();
    while Instant::now() < deadline {
        let r = guest
            .call(
                "view.evaluateJavascript",
                json!({"view": view, "script": "document.getElementById('status').textContent"}),
            )
            .expect("status read");
        status = r["valueJson"].as_str().unwrap_or("").to_string();
        if status.contains("tapped") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    (ack, status)
}

/// What one arm measured. Every field is asserted on, not just recorded.
struct Arm {
    fast: PullSeries,
    slow: PullSeries,
    /// Wall time for the arm's concurrent pull burst (the coalescing arm).
    burst_ms: Option<u128>,
    tap_ack_ms: u128,
    tap_status: String,
    frames: serde_json::Value,
}

impl Arm {
    fn report(&self) -> serde_json::Value {
        json!({
            "fast": self.fast.report(),
            "slow": self.slow.report(),
            "burstMs": self.burst_ms,
            "tapAckMs": self.tap_ack_ms,
            "tapStatus": self.tap_status,
            "gate": self.frames,
        })
    }
}

/// The budgets. Each one is a *bound* the gate promises, set from the measured
/// comparison against the pre-gate broker (`evidence/fair-pulls-before.txt`):
/// the pre-gate numbers are in the failure messages so a regression says what
/// it regressed from.
const FAST_MEDIAN_MS: u128 = 400;
const FAST_P95_MS: u128 = 1200;
const FAST_MAX_MS: u128 = 1500;
const TAP_ACK_MS: u128 = 1500;
/// Eight concurrent pulls of the slow view, coalesced to one engine cycle.
const BURST_MS: u128 = 12_000;

fn fast_guest_status(guest: &WireGuest) -> serde_json::Value {
    guest
        .call("engineStatus", json!({}))
        .map(|v| v["frames"].clone())
        .unwrap_or(serde_json::Value::Null)
}

/// Two views (a fast one and a slow one) pulled at the same time by one thread
/// each, with a tap in the middle. The brief's case.
fn measure_two_views(
    fast_guest: Arc<WireGuest>,
    fast_view: i64,
    slow_guest: Arc<WireGuest>,
    slow_view: i64,
    tag: &str,
) -> Arm {
    let stop = Arc::new(AtomicBool::new(false));
    let slow = {
        let guest = Arc::clone(&slow_guest);
        let stop = Arc::clone(&stop);
        let label = format!("{tag} slow");
        std::thread::spawn(move || pull_loop(guest, slow_view, stop, &label))
    };
    let fast = {
        let guest = Arc::clone(&fast_guest);
        let stop = Arc::clone(&stop);
        let label = format!("{tag} fast");
        std::thread::spawn(move || pull_loop(guest, fast_view, stop, &label))
    };
    // Let the two pulls establish contention, then tap the fast view while the
    // slow page still holds the engine: the frames the tap needs must not queue
    // behind the slow page's.
    std::thread::sleep(Duration::from_millis(900));
    let (tap_ack_ms, tap_status) = tap_and_verify(&fast_guest, fast_view);
    std::thread::sleep(Duration::from_millis(400));
    stop.store(true, Ordering::SeqCst);
    let slow = slow.join().expect("slow pull loop");
    let fast = fast.join().expect("fast pull loop");
    let frames = fast_guest_status(&fast_guest);
    Arm {
        fast,
        slow,
        burst_ms: None,
        tap_ack_ms,
        tap_status,
        frames,
    }
}

/// Eight guest requests for the *same* view at once. One view has exactly one
/// frame being produced, however many requests name it (bounded in-flight); a
/// second view's pulls must not queue behind all eight.
fn measure_coalesced_burst(
    fast_guest: Arc<WireGuest>,
    fast_view: i64,
    slow_guest: Arc<WireGuest>,
    slow_view: i64,
    tag: &str,
) -> Arm {
    let stop = Arc::new(AtomicBool::new(false));
    let fast = {
        let guest = Arc::clone(&fast_guest);
        let stop = Arc::clone(&stop);
        let label = format!("{tag} fast");
        std::thread::spawn(move || pull_loop(guest, fast_view, stop, &label))
    };
    std::thread::sleep(Duration::from_millis(300));
    let t0 = Instant::now();
    let burst = {
        let guest = Arc::clone(&slow_guest);
        let mut handles = Vec::new();
        for i in 0..8 {
            let guest = Arc::clone(&guest);
            handles.push(std::thread::spawn(move || {
                let t = Instant::now();
                let (_meta, blob) = guest.render(slow_view);
                assert_png(&blob.data);
                (i, t.elapsed().as_millis())
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().expect("burst puller"))
            .collect::<Vec<_>>()
    };
    let burst_ms = t0.elapsed().as_millis();
    let (tap_ack_ms, tap_status) = tap_and_verify(&fast_guest, fast_view);
    std::thread::sleep(Duration::from_millis(200));
    stop.store(true, Ordering::SeqCst);
    let fast = fast.join().expect("fast pull loop");
    let frames = fast_guest_status(&fast_guest);
    Arm {
        fast,
        slow: PullSeries {
            label: format!("{tag} slow x8 concurrent"),
            elapsed: burst.iter().map(|(_, ms)| *ms).collect(),
        },
        burst_ms: Some(burst_ms),
        tap_ack_ms,
        tap_status,
        frames,
    }
}

/// Four slow views pulled concurrently in **one session**, then a tap on a fast
/// view of that session. The connection has a small shared worker pool; frame
/// pulls must not occupy it, or the tap's request queues behind four slow
/// renders (measured before the gate: a 5.7 s ack).
fn measure_worker_pool(
    guest: Arc<WireGuest>,
    fast_view: i64,
    slow_views: &[i64],
    tag: &str,
) -> Arm {
    let stop = Arc::new(AtomicBool::new(false));
    let mut slow_threads = Vec::new();
    for view in slow_views {
        let guest = Arc::clone(&guest);
        let stop = Arc::clone(&stop);
        let label = format!("{tag} slow {view}");
        let view = *view;
        slow_threads.push(std::thread::spawn(move || {
            pull_loop(guest, view, stop, &label)
        }));
    }
    let fast = {
        let guest = Arc::clone(&guest);
        let stop = Arc::clone(&stop);
        let label = format!("{tag} fast");
        std::thread::spawn(move || pull_loop(guest, fast_view, stop, &label))
    };
    std::thread::sleep(Duration::from_millis(900));
    let (tap_ack_ms, tap_status) = tap_and_verify(&guest, fast_view);
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::SeqCst);
    let mut slow = PullSeries {
        label: format!("{tag} slow views"),
        elapsed: Vec::new(),
    };
    for t in slow_threads {
        slow.elapsed.extend(t.join().expect("slow pull loop").elapsed);
    }
    let fast = fast.join().expect("fast pull loop");
    let frames = fast_guest_status(&guest);
    Arm {
        fast,
        slow,
        burst_ms: None,
        tap_ack_ms,
        tap_status,
        frames,
    }
}

fn assert_fast_view_not_starved(arm: &Arm, tag: &str) {
    assert!(
        arm.fast.elapsed.len() >= 6,
        "{tag}: only {} fast pulls measured; the arm did not exercise contention",
        arm.fast.elapsed.len()
    );
    let mut fast = arm.fast.elapsed.clone();
    fast.sort_unstable();
    let median = percentile(&fast, 0.50);
    let p95 = percentile(&fast, 0.95);
    let max = fast.last().copied().unwrap_or(0);
    assert!(
        median <= FAST_MEDIAN_MS,
        "{tag}: fast view median pull {median} ms (budget {FAST_MEDIAN_MS}): the slow \
         page is still serialising every pull"
    );
    assert!(
        p95 <= FAST_P95_MS,
        "{tag}: fast view p95 pull {p95} ms (budget {FAST_P95_MS})"
    );
    assert!(
        max <= FAST_MAX_MS,
        "{tag}: fast view worst pull {max} ms (budget {FAST_MAX_MS}); the pre-gate \
         broker measured 1943 ms here (one slow pull held the engine for 5.7 s)"
    );
}

fn assert_tap_landed(arm: &Arm, tag: &str) {
    assert!(
        arm.tap_ack_ms <= TAP_ACK_MS,
        "{tag}: tap ack {} ms while a slow page pulled (budget {TAP_ACK_MS}); the \
         pre-gate broker measured 1085 ms with one slow page and ~5.7 s with four",
        arm.tap_ack_ms
    );
    assert!(
        arm.tap_status.contains("tapped"),
        "{tag}: the tap never reached the page (status {:?})",
        arm.tap_status
    );
}

#[test]
fn frame_pulls_are_fair_between_two_views_one_of_them_slow() {
    if !engine_test_enabled() {
        println!(
            "SKIP fair_pulls: set ACOS_WEBVIEW_TEST=1 (and run it under \
             tools/art-runtime/run-exclusive.sh) to measure against the real engine"
        );
        return;
    }
    let root = scratch("two-views");
    let cache = EngineCache::default_cache();
    let mut cfg = BrokerConfig::new(EngineCache::new(cache.root()));
    cfg.run_root = root.join("run");
    cfg.idle_shutdown = Duration::from_secs(120);
    let broker = Broker::new(cfg);
    let socket = root.join("run").join("webview.sock");
    let server = serve_shared(
        Arc::clone(&broker),
        &socket,
        SharedConfig { idle_exit: None },
    )
    .expect("serve_shared");
    eprintln!("broker socket: {}", socket.display());

    // Two sessions on the one host-wide broker, attached exactly like `gate`.
    let a = Arc::new(WireGuest::connect_attached(&socket, "fair-a"));
    let b = Arc::new(WireGuest::connect_attached(&socket, "fair-b"));
    a.hello();
    b.hello();

    let mut report = json!({
        "test": "fair_pulls",
        "viewport": {"width": WIDTH, "height": HEIGHT, "density": DENSITY},
        "budgets": {
            "fastMedianMs": FAST_MEDIAN_MS,
            "fastP95Ms": FAST_P95_MS,
            "fastMaxMs": FAST_MAX_MS,
            "tapAckMs": TAP_ACK_MS,
            "burstMs": BURST_MS,
        },
        "engine": cache.cached().map(|v| json!({
            "version": acos_webview::ENGINE_VERSION,
            "zipSha256": v.zip_sha256,
            "binarySha256": v.binary_sha256,
        })),
    });

    // --- arm 1: one slow view, one fast view, two sessions ----------------
    let a_fast = open_view(&a, 1, FAST_PAGE, "session a fast");
    if arm_selected("two_sessions") {
        let b_slow = open_view(&b, 1, SLOW_PAGE, "session b slow");
        start_slow_loop(&b, b_slow);
        let arm = measure_two_views(Arc::clone(&a), a_fast, Arc::clone(&b), b_slow, "two_sessions");
        assert_fast_view_not_starved(&arm, "two_sessions");
        assert_tap_landed(&arm, "two_sessions");
        report["two_sessions"] = arm.report();
        eprintln!("two_sessions: {}", report["two_sessions"]);
        let _ = b.call("view.destroy", json!({"view": b_slow}));
    }

    // --- arm 2: the same two pages, both views in one session -------------
    let a_fast2 = open_view(&a, 2, FAST_PAGE, "session a fast #2");
    let a_slow2 = open_view(&a, 3, SLOW_PAGE, "session a slow #2");
    start_slow_loop(&a, a_slow2);
    if arm_selected("one_session") {
        let arm = measure_two_views(Arc::clone(&a), a_fast2, Arc::clone(&a), a_slow2, "one_session");
        assert_fast_view_not_starved(&arm, "one_session");
        assert_tap_landed(&arm, "one_session");
        report["one_session"] = arm.report();
        eprintln!("one_session: {}", report["one_session"]);
    }

    // --- arm 3: eight concurrent pulls of one slow view -------------------
    if arm_selected("coalesced_burst") {
        let arm = measure_coalesced_burst(
            Arc::clone(&a),
            a_fast2,
            Arc::clone(&a),
            a_slow2,
            "coalesced_burst",
        );
        let burst = arm.burst_ms.expect("burst arm measured its wall time");
        assert!(
            burst <= BURST_MS,
            "coalesced_burst: eight concurrent pulls of one slow view took {burst} ms \
             (budget {BURST_MS}); the pre-gate broker ran them one after another \
             (8 x 5.7 s) because nothing coalesced per view"
        );
        assert_tap_landed(&arm, "coalesced_burst");
        report["coalesced_burst"] = arm.report();
        eprintln!("coalesced_burst: {}", report["coalesced_burst"]);
    }

    // --- arm 4: four slow views + the fast view, one session --------------
    let mut slow_views = Vec::new();
    if arm_selected("worker_pool") {
        for id in 10..14 {
            let view = open_view(&a, id, SLOW_PAGE, "session a slow pool");
            start_slow_loop_with(&a, view, START_SLOW_POOL);
            slow_views.push(view);
        }
        let arm = measure_worker_pool(Arc::clone(&a), a_fast2, &slow_views, "worker_pool");
        // The fast view's own pulls may queue behind four waiting views (fair
        // FIFO), so the tap — whose frames are input priority, and whose request
        // must not queue behind four frame pulls in the connection's workers —
        // is what is bounded here.
        assert_tap_landed(&arm, "worker_pool");
        assert!(
            !arm.slow.elapsed.is_empty(),
            "worker_pool: the slow views never finished a pull"
        );
        assert!(
            arm.fast.elapsed.len() >= 6,
            "worker_pool: only {} fast pulls measured",
            arm.fast.elapsed.len()
        );
        report["worker_pool"] = arm.report();
        eprintln!("worker_pool: {}", report["worker_pool"]);
    }

    // --- evidence ------------------------------------------------------
    let dir = std::env::var_os("ACOS_WEBVIEW_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(|p| p.parent())
                .map(|root| root.join("results/handoff/WVFAIR/evidence"))
                .unwrap_or_else(|| PathBuf::from("evidence"))
        });
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("fair-pulls.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&report).expect("report json"))
        .expect("write evidence");
    eprintln!("evidence: {}", path.display());

    // Tear down like a session end: the views, then the sockets.
    for view in [a_fast, a_fast2, a_slow2] {
        let _ = a.call("view.destroy", json!({"view": view}));
    }
    for view in &slow_views {
        let _ = a.call("view.destroy", json!({"view": view}));
    }
    a.close();
    b.close();
    drop(server);
    let _ = std::fs::remove_dir_all(&root);

    // An engine that crashed under the slow pages would not be a pass.
    assert!(
        broker.restarts() == 0,
        "the engine restarted {} times under the two-view arms",
        broker.restarts()
    );
}

/// The arm's own geometry helper is worth asserting: the wire's display pixels
/// are the CSS pixels × density (`WIRE.md` §4.2 / ADR-0031's frame decision).
#[test]
fn viewport_geometry_matches_the_probe() {
    let vp = Viewport::new(WIDTH, HEIGHT, DENSITY).expect("viewport");
    assert_eq!(vp.width, 900);
    assert_eq!((vp.width as f64 / vp.density).round(), 343.0);
}
