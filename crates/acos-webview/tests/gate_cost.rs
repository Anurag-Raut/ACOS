//! What the lazy gate costs a session that never creates a `WebView` (WV5b
//! acceptance item 3): the listener's added start time and PSS, measured on
//! **this** process — the same thing a launch pays, because `launch_session`
//! does exactly this call and nothing else.
//!
//! Fast and host-only (no engine, no daemon, no lock): five gates are started
//! the way a session starts one, and each one's cost is read from
//! `/proc/self/smaps_rollup` and `/proc/self/status` before and after. The
//! assertion budgets are real limits, not the measurement's own numbers:
//!
//! * start: < 50 ms per gate (measured in hundreds of microseconds),
//! * PSS: < 1 MiB per gate,
//! * threads: +1 per gate (the accept thread) and nothing else,
//! * processes: no daemon, no engine, no spawn at all.
//!
//! The session-level numbers (`session_start_ms` and launcher PSS, with and
//! without the gate) are in `results/handoff/WV5b/REPORT.md` §3.

mod common;

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use acos_webview::gate::{BrokerSource, Gate, GateConfig};

#[derive(Debug, Clone, Copy, Default)]
struct ProcCost {
    pss_kib: u64,
    threads: u64,
    rss_kib: u64,
}

fn read_cost() -> ProcCost {
    let mut cost = ProcCost::default();
    if let Ok(rollup) = std::fs::read_to_string("/proc/self/smaps_rollup") {
        for line in rollup.lines() {
            if let Some(rest) = line.strip_prefix("Pss:") {
                cost.pss_kib = rest
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
            }
            if let Some(rest) = line.strip_prefix("Rss:") {
                cost.rss_kib = rest
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
            }
        }
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        cost.threads = status
            .lines()
            .find(|l| l.starts_with("Threads:"))
            .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
            .unwrap_or(0);
    }
    cost
}

fn scratch(tag: &str) -> PathBuf {
    // Absolute and **short**: a session socket path must stay well inside
    // `sun_path`'s 107 bytes even in a deep worktree, and the broker endpoint a
    // test names must not depend on the daemon's own cwd.
    let base = std::env::var_os("ACOS_VIEW_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("acos-wv5b-{}", std::process::id()))
        });
    let dir = base.join(format!("c{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

#[test]
fn a_session_that_never_connects_pays_a_socket_and_a_thread() {
    let root = scratch("gate");
    // The endpoint must not exist: if the gate ever tried to reach it, the
    // connect would fail and (with External) the gate would only fail on the
    // first guest connect — which never comes here.
    let broker_socket = root.join("run/broker.sock");
    let before = read_cost();
    let mut starts = Vec::new();
    let mut gates = Vec::new();
    for i in 0..5 {
        let dir = root.join(format!("s{i}"));
        let t0 = Instant::now();
        let gate = Gate::start(GateConfig {
            session_dir: dir,
            session_id: format!("s{i}"),
            broker_socket: broker_socket.clone(),
            broker: BrokerSource::External,
            engine_cache: None,
            idle_shutdown: None,
            log: None,
            spawn_wait: Duration::from_millis(200),
        })
        .expect("gate start");
        starts.push(t0.elapsed());
        gates.push(gate);
    }
    let after = read_cost();
    let pss_delta_kib = after.pss_kib.saturating_sub(before.pss_kib);
    let threads_delta = after.threads.saturating_sub(before.threads);
    let max_start = starts.iter().max().copied().unwrap_or_default();
    let total_ms: f64 = starts.iter().map(|d| d.as_secs_f64() * 1000.0).sum();
    let report = format!(
        "gate cost: 5 gates, start per gate {:?} (max {max_start:?}, total {total_ms:.3} ms), \
         pss +{pss_delta_kib} KiB (per gate {:.1} KiB), threads +{threads_delta}, \
         processes +0 (no daemon, no engine)\n",
        starts,
        pss_delta_kib as f64 / 5.0
    );
    print!("{report}");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("gate-cost.txt"))
    {
        let _ = f.write_all(report.as_bytes());
    }

    // Nothing woke up: no daemon, no engine, no spawned process.
    for (i, gate) in gates.iter().enumerate() {
        assert_eq!(gate.stats().broker_spawns, 0, "gate {i} must not spawn a broker");
        assert_eq!(gate.stats().connects, 0);
        assert!(!gate.attached());
        assert!(gate.socket().exists(), "gate {i} must be listening");
    }
    assert!(!broker_socket.exists(), "no broker endpoint before a connect");
    assert!(
        max_start < Duration::from_millis(50),
        "a gate must start in well under the 50 ms budget: {max_start:?}"
    );
    assert!(
        pss_delta_kib < 5 * 1024,
        "5 idle gates must stay under 1 MiB PSS each, measured +{pss_delta_kib} KiB total"
    );
    assert!(
        threads_delta <= 5,
        "one accept thread per gate and nothing else, measured +{threads_delta}"
    );

    // Teardown: every session directory goes away with its gate.
    drop(gates);
    for i in 0..5 {
        assert!(!root.join(format!("s{i}")).exists());
    }
    let _ = std::fs::remove_dir_all(&root);
}
