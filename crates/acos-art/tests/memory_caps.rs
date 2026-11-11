//! The R1 regression suite: a guest that spams cannot grow the launcher.
//!
//! On 2026-11-11 `acos-art` was OOM-killed three times — 6.2 GB inside a 6 GB
//! guard, then 36 GB anon-rss unguarded, which took the whole desktop session
//! and every running agent with it. The reproduction was a `session` run whose
//! guest filled the in-process logd sink faster than anything drained it, so the
//! host retained every entry forever (see `results/art-runtime/R1.md`).
//!
//! These tests are the permanent proof that the ceilings hold, without needing a
//! real ART guest: a spammer is a child process that never stops writing, and a
//! log spammer is a unix datagram socket fed in a loop. Every test asserts on
//! **retained bytes** and on **this process's RSS**, so a regression that keeps
//! the API but loses the bound fails here.

use acos_art::capture::{drain_to_tail, TailBuffer, DEFAULT_LOG_CAP, DEFAULT_STREAM_CAP};
use acos_art::logd::Logd;
use std::io::Read;
use std::os::unix::net::UnixDatagram;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// This process's RSS in MiB (`/proc/self/status:VmRSS`).
fn self_rss_mb() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
    let kb: f64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse().ok())
        .expect("VmRSS is present");
    kb / 1024.0
}

/// A guest process that writes to stdout/stderr forever.
///
/// `yes` is the honest stand-in for a looping `System.out.println`/`printf`: it
/// produces bytes as fast as the pipe accepts them, and only the host's capture
/// policy decides how much of it survives. Nothing here is capped by the child.
fn spawn_spammer() -> std::process::Child {
    Command::new("/bin/sh")
        .arg("-c")
        .arg("yes 'acos-art spammer: a line a guest could loop on forever' 2>/dev/null")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("/bin/sh is available")
}

/// Drain a pipe for `budget`, then report what was retained. The reader is the
/// same helper the three launch paths use, so this is not a parallel code path
/// that could drift from production.
fn drain_for(pipe: impl Read, cap: usize, budget: Duration) -> TailBuffer {
    let start = Instant::now();
    let mut tail = TailBuffer::with_cap(cap);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut reader = pipe;
    while start.elapsed() < budget {
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => tail.push(&chunk[..n]),
        }
    }
    tail
}

#[test]
fn stdout_spammer_is_capped_and_the_host_rss_stays_flat() {
    let cap = 1024 * 1024; // 1 MiB: small enough to trip inside the budget
    let rss_before = self_rss_mb();
    let mut child = spawn_spammer();
    let out = child.stdout.take().expect("piped stdout");
    let tail = drain_for(out, cap, Duration::from_millis(1500));
    let rss_after = self_rss_mb();
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        tail.total() > 8 * cap as u64,
        "the spammer must actually produce far more than the cap (produced {} bytes)",
        tail.total()
    );
    assert!(
        tail.len() <= cap + 64 * 1024,
        "retained {} bytes for a {cap}-byte cap",
        tail.len()
    );
    assert!(tail.truncated(), "a capped capture must know it was truncated");
    assert_eq!(tail.len() as u64 + tail.dropped(), tail.total());
    assert!(
        tail.to_text().starts_with("[acos-art: capture truncated:"),
        "the capture must say so in-band"
    );
    // 64 MiB of spam through a 1 MiB cap: RSS may jitter, it may not grow by the
    // size of the stream. (The old `read_to_end` grew by exactly `total`.)
    assert!(
        rss_after - rss_before < 32.0,
        "host RSS grew {:.1} MiB while capturing {} MiB of spam",
        rss_after - rss_before,
        tail.total() / 1024 / 1024
    );
}

#[test]
fn stderr_spammer_is_capped_too() {
    // stderr and stdout go through the same helper; this proves the *stderr*
    // half of the pair (the incident's session path kept stderr in a String).
    let cap = 512 * 1024;
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg("yes 'stderr spam' 1>&2 2>/dev/null")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("/bin/sh is available");
    let err = child.stderr.take().expect("piped stderr");
    let tail = drain_for(err, cap, Duration::from_millis(1000));
    let _ = child.kill();
    let _ = child.wait();
    assert!(tail.total() > 4 * cap as u64, "produced {}", tail.total());
    assert!(tail.len() <= cap + 64 * 1024);
    assert!(tail.truncated());
}

#[test]
fn logd_spammer_keeps_only_the_cap_and_counts_the_drops() {
    // The incident itself: the guest's liblog loop filling the in-process sink.
    let dir = std::env::temp_dir().join(format!("acos-art-r1-logd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cap = 500;
    let logd = Logd::start_with_cap(&dir, cap).expect("bind logdw");
    let path = logd.path().to_path_buf();
    let sock = UnixDatagram::unbound().expect("unbound datagram socket");
    sock.connect(&path).expect("connect logdw");

    let rss_before = self_rss_mb();
    let msg = vec![b'x'; 900]; // a ~1 KiB entry, like a liblog line
    for i in 0..60_000u32 {
        let mut f = vec![6u8, 3, 0];
        f.extend_from_slice(&(msg.len() as u16).to_le_bytes());
        f.push(0);
        f.extend_from_slice(b"art");
        f.extend_from_slice(&msg);
        let _ = sock.send(&f);
        if i % 128 == 0 {
            std::thread::sleep(Duration::from_micros(40));
        }
    }
    std::thread::sleep(Duration::from_millis(250));
    let batch = logd.finish_capped();
    let rss_after = self_rss_mb();

    assert!(
        batch.entries.len() <= cap,
        "logd kept {} entries for a {cap}-entry cap",
        batch.entries.len()
    );
    assert_eq!(batch.stats.cap, cap);
    assert_eq!(
        batch.stats.total,
        batch.stats.kept as u64 + batch.stats.dropped,
        "every entry is either kept or counted as dropped"
    );
    assert!(
        batch.stats.total > 10 * cap as u64,
        "the spammer must trip the cap (saw {} entries)",
        batch.stats.total
    );
    assert!(batch.stats.dropped > 0);
    assert!(batch.stats.summary().contains("dropped"), "{}", batch.stats.summary());
    assert!(
        rss_after - rss_before < 32.0,
        "host RSS grew {:.1} MiB across {} logd entries",
        rss_after - rss_before,
        batch.stats.total
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_cap_is_smaller_than_the_watchdog_ceiling() {
    // Defence in depth only works if the caps sit *below* the watchdog: the caps
    // are what should stop a spammer, and the watchdog is the last resort.
    assert!(DEFAULT_STREAM_CAP * 2 < 2 * 1024 * 1024 * 1024);
    assert!(DEFAULT_LOG_CAP > 0);
    // The watchdog default is the documented 2 GiB launcher ceiling.
    assert_eq!(acos_art::watchdog::DEFAULT_RSS_MAX, 2 * 1024 * 1024 * 1024);
}

#[test]
fn stream_cap_is_configurable_but_never_absent() {
    // A deliberate experiment may raise or lower the ceiling; it may not remove
    // it (a `0` falls back to the default rather than to "unbounded").
    std::env::set_var("ACOS_ART_STREAM_CAP", "0");
    assert_eq!(acos_art::capture::stream_cap_from_env(), DEFAULT_STREAM_CAP);
    std::env::set_var("ACOS_ART_STREAM_CAP", "1M");
    assert_eq!(acos_art::capture::stream_cap_from_env(), 1024 * 1024);
    std::env::remove_var("ACOS_ART_STREAM_CAP");
}

#[test]
fn drain_to_tail_on_a_finite_reader_keeps_the_whole_stream() {
    let data: Vec<u8> = (0..1024u32).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
    let tail = drain_to_tail(&data[..], DEFAULT_STREAM_CAP);
    assert_eq!(tail.len(), data.len());
    assert!(!tail.truncated());
    assert_eq!(tail.to_text(), String::from_utf8_lossy(&data));
}
