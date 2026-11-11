//! The launcher's own memory ceiling — defence in depth around the caps.
//!
//! The per-buffer caps ([`crate::capture`], [`crate::logd`]) stop the *known*
//! growth paths. This module stops the unknown ones: a memory watchdog inside
//! `acos-art` that samples its own RSS and, when a ceiling is crossed, kills the
//! whole guest process group and reports a structured `HOST_MEMORY_LIMIT`
//! instead of letting the kernel OOM killer pick a victim (on 2026-11-11 that
//! victim was the desktop session: an unguarded `acos-art` reached 36 GB
//! anon-rss and took every running agent down with it — see
//! `results/art-runtime/R1.md`).
//!
//! Three layers, cheapest-to-degrade first:
//!
//! 1. **Caps** — bounded captures and bounded logd/peer queues (the guest cannot
//!    make the host allocate without limit in the first place).
//! 2. **Watchdog** — `ACOS_ART_RSS_MAX` (default 2 GiB, `0` disables): `/proc/self/status:VmRSS`
//!    sampled every [`POLL_INTERVAL`]; on breach the guest process group gets
//!    `SIGKILL` and the run ends with code `HOST_MEMORY_LIMIT`.
//! 3. **Self rlimit** — a soft `RLIMIT_DATA` (default `ACOS_ART_RSS_MAX * 5/4`,
//!    hard unlimited so children can raise it) so an allocation storm that the
//!    watchdog has not sampled yet still fails inside the launcher instead of
//!    eating the machine. `ACOS_ART_RLIMIT_DATA` overrides it, `0` disables it.
//!
//! The launcher keeps the rlimit *soft* and every guest `pre_exec` raises it
//! back to unlimited ([`relax_child_limits`]): ART is not the process being
//! bounded here, and Scudo reserves ~8.6 TB of virtual address space at startup.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::capture::parse_bytes;

/// Default ceiling for the launcher's own RSS: 2 GiB.
pub const DEFAULT_RSS_MAX: u64 = 2 * 1024 * 1024 * 1024;

/// How often the watchdog samples `/proc/self/status`. 50 ms bounds the overshoot
/// of a fast leak to a few tens of MiB while costing ~10 µs per sample.
pub const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The structured code a watchdog kill reports.
pub const HOST_MEMORY_LIMIT: &str = "HOST_MEMORY_LIMIT";

/// The RSS ceiling from `ACOS_ART_RSS_MAX` (bytes, `K`/`M`/`G` suffixes) or
/// [`DEFAULT_RSS_MAX`]. `0` disables the watchdog.
#[must_use]
pub fn rss_max_from_env() -> u64 {
    std::env::var("ACOS_ART_RSS_MAX")
        .ok()
        .and_then(|s| parse_bytes(&s))
        .map(|v| v as u64)
        .unwrap_or(DEFAULT_RSS_MAX)
}

/// The soft `RLIMIT_DATA` for the launcher from `ACOS_ART_RLIMIT_DATA`, or
/// `rss_max + rss_max/4` so the structured watchdog (`HOST_MEMORY_LIMIT`) always
/// has room to fire and report before malloc fails. `0` (or a disabled RSS cap)
/// disables the rlimit too.
#[must_use]
pub fn data_limit_from_env(rss_max: u64) -> u64 {
    if let Ok(raw) = std::env::var("ACOS_ART_RLIMIT_DATA") {
        return parse_bytes(&raw).map(|v| v as u64).unwrap_or(0);
    }
    if rss_max == 0 {
        0
    } else {
        rss_max + rss_max / 4
    }
}

/// A watchdog trip: the RSS that crossed the ceiling and what was killed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trip {
    pub rss_bytes: u64,
    pub limit_bytes: u64,
    /// The process group the watchdog killed (`None` if it had not been armed
    /// with one yet).
    pub killed_pgid: Option<i32>,
}

/// Read this process's RSS from `/proc/self/status:VmRSS` (bytes).
#[must_use]
pub fn read_self_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

/// The launcher's memory watchdog.
///
/// Dropping it stops the sampler. A trip kills the armed process group
/// immediately (the guest is the reason the launcher is holding memory) and
/// records the structured evidence for the caller's result.
pub struct MemoryWatchdog {
    limit_bytes: u64,
    target: Arc<Mutex<Option<i32>>>,
    trip: Arc<Mutex<Option<Trip>>>,
    rss_now: Arc<Mutex<u64>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MemoryWatchdog {
    /// Start a watchdog with an explicit ceiling (`0` ⇒ `None`: disabled).
    #[must_use]
    pub fn start(limit_bytes: u64) -> Option<MemoryWatchdog> {
        if limit_bytes == 0 {
            return None;
        }
        let target: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
        let trip: Arc<Mutex<Option<Trip>>> = Arc::new(Mutex::new(None));
        let rss_now = Arc::new(Mutex::new(0u64));
        let stop = Arc::new(AtomicBool::new(false));
        let (t, tr, rn, s) = (
            Arc::clone(&target),
            Arc::clone(&trip),
            Arc::clone(&rss_now),
            Arc::clone(&stop),
        );
        let handle = thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                if let Some(rss) = read_self_rss_bytes() {
                    *rn.lock().unwrap() = rss;
                    if rss > limit_bytes {
                        let pgid = *t.lock().unwrap();
                        if let Some(pgid) = pgid {
                            unsafe {
                                // The whole guest group: bwrap + dalvikvm64 +
                                // every ART thread it started.
                                libc::kill(-pgid, libc::SIGKILL);
                                libc::kill(pgid, libc::SIGKILL);
                            }
                        }
                        *tr.lock().unwrap() = Some(Trip {
                            rss_bytes: rss,
                            limit_bytes,
                            killed_pgid: pgid,
                        });
                        eprintln!(
                            "acos-art: HOST_MEMORY_LIMIT: launcher RSS {} MiB > {} MiB (ACOS_ART_RSS_MAX); \
                             killed guest process group {:?}",
                            rss / 1024 / 1024,
                            limit_bytes / 1024 / 1024,
                            pgid
                        );
                        // Armed: the kill ends the growth, so stop sampling. Not
                        // armed (a guard installed before any guest exists): keep
                        // watching silently in case a run path arms it later; the
                        // soft RLIMIT_DATA backstop bounds the worst case.
                        if pgid.is_some() {
                            break;
                        }
                    }
                }
                thread::sleep(POLL_INTERVAL);
            }
        });
        Some(Self { limit_bytes, target, trip, rss_now, stop, handle: Some(handle) })
    }

    /// Start from `ACOS_ART_RSS_MAX` (the normal path).
    #[must_use]
    pub fn from_env() -> Option<MemoryWatchdog> {
        Self::start(rss_max_from_env())
    }

    /// Arm the watchdog with the guest's process group id (called right after
    /// `spawn`, because that is when the pgid exists).
    pub fn arm_process_group(&self, pgid: i32) {
        *self.target.lock().unwrap() = Some(pgid);
    }

    /// The trip record, if the ceiling was crossed.
    #[must_use]
    pub fn trip(&self) -> Option<Trip> {
        *self.trip.lock().unwrap()
    }

    /// The last sampled RSS (bytes; `0` before the first sample).
    #[must_use]
    pub fn rss_bytes(&self) -> u64 {
        *self.rss_now.lock().unwrap()
    }

    /// The configured ceiling.
    #[must_use]
    pub fn limit_bytes(&self) -> u64 {
        self.limit_bytes
    }

    /// A one-line diagnostic describing the trip (never silent about a kill).
    #[must_use]
    pub fn diagnostic(&self) -> Option<String> {
        self.trip().map(|t| {
            format!(
                "{HOST_MEMORY_LIMIT}: launcher RSS {} MiB exceeded the {} MiB ceiling \
                 (ACOS_ART_RSS_MAX); guest process group {:?} killed",
                t.rss_bytes / 1024 / 1024,
                t.limit_bytes / 1024 / 1024,
                t.killed_pgid
            )
        })
    }
}

impl Drop for MemoryWatchdog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Apply the launcher's own soft `RLIMIT_DATA` backstop. Returns the applied
/// limit (`0` = not applied), never an error: an unavailable rlimit must not stop
/// a run that the caps and the watchdog already protect.
#[must_use]
pub fn install_self_data_limit(rss_max: u64) -> u64 {
    let limit = data_limit_from_env(rss_max);
    if limit == 0 {
        return 0;
    }
    let rl = libc::rlimit { rlim_cur: limit as libc::rlim_t, rlim_max: libc::RLIM_INFINITY };
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_DATA, &rl) };
    if rc == 0 {
        limit
    } else {
        eprintln!(
            "acos-art: could not set the RLIMIT_DATA backstop to {} MiB ({})",
            limit / 1024 / 1024,
            std::io::Error::last_os_error()
        );
        0
    }
}

/// Raise the launcher's soft `RLIMIT_DATA`/`RLIMIT_AS` back to unlimited in a
/// child before `exec`. Called from every guest `pre_exec`: the ceilings bound
/// *acos-art*, not ART (which is why they are soft — a hard limit would be
/// inherited and could not be raised here).
pub fn relax_child_limits() {
    for resource in [libc::RLIMIT_DATA, libc::RLIMIT_AS] {
        let mut cur = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        if unsafe { libc::getrlimit(resource, &mut cur) } != 0 {
            continue;
        }
        if cur.rlim_cur == cur.rlim_max {
            continue; // nothing to raise (already unlimited or hard-capped)
        }
        let unlimited = libc::rlimit { rlim_cur: cur.rlim_max, rlim_max: cur.rlim_max };
        unsafe {
            libc::setrlimit(resource, &unlimited);
        }
    }
}

/// Stock Android's `init.rc` sets `setrlimit nice 40 40` ("allow priorities
/// from 19 to -20") for every service. That is what lets an app thread call
/// `android.os.Process.setThreadPriority(THREAD_PRIORITY_DISPLAY)` (-4):
/// `androidSetThreadPriority` does `setpriority(PRIO_PROCESS, tid, -4)` and
/// libandroid_runtime turns EACCES into `SecurityException`. With the Linux
/// (systemd) default hard limit of 0 the call is refused, and e.g. RN 0.81's
/// `ReactInstanceManager.create_react_context` thread dies before the JS
/// runtime exists (measured in W20).
pub const INIT_RLIMIT_NICE: libc::rlim_t = 40;

/// Mirror `init.rc`'s `RLIMIT_NICE` in a forked child (`pre_exec`).
///
/// Raising the *hard* limit needs `CAP_SYS_RESOURCE` in the initial user
/// namespace, so on an unprivileged host (hard limit 0) this is a no-op and
/// must never be fatal. A host that starts the engine with the stock limit
/// gets stock behaviour with no further change.
pub fn apply_init_rlimit_nice() {
    let lim = libc::rlimit { rlim_cur: INIT_RLIMIT_NICE, rlim_max: INIT_RLIMIT_NICE };
    unsafe {
        libc::setrlimit(libc::RLIMIT_NICE, &lim);
    }
}

/// The stock-priority gate: a nice value of -4 (display priority) needs
/// `RLIMIT_NICE >= 24`. Read the host's hard limit so a route can say, once,
/// that `Process.setThreadPriority(-4)` will fail on this host instead of
/// letting the guest fail with a bare `SecurityException`.
#[must_use]
pub fn guest_priority_supported() -> bool {
    let mut cur = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::getrlimit(libc::RLIMIT_NICE, &mut cur) } != 0 {
        return false;
    }
    cur.rlim_max >= 24
}

/// Print the stock-priority gap once per launcher process (the routes call it
/// right before spawning the guest). `init.rc`'s hard value is 40; the Linux
/// default user session value is 0.
pub fn note_guest_priority_gap(route: &str) {
    static NOTED: AtomicBool = AtomicBool::new(false);
    if guest_priority_supported() || NOTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let mut cur = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    unsafe {
        libc::getrlimit(libc::RLIMIT_NICE, &mut cur);
    }
    eprintln!(
        "acos-art: {route}: guest RLIMIT_NICE {} < init.rc {INIT_RLIMIT_NICE}: \
         Process.setThreadPriority(THREAD_PRIORITY_DISPLAY) will throw SecurityException \
         (start the runner with a stock nice limit >= 24 / CAP_SYS_RESOURCE in the initial userns)",
        cur.rlim_max
    );
}

/// Install the launcher-side ceilings once per process (idempotent enough to be
/// called from `main`): the soft rlimit backstop plus a process-wide watchdog
/// that stays armed for the whole invocation.
///
/// Returns `(watchdog, applied_data_limit)` so the caller can report both.
#[must_use]
pub fn install_process_guards() -> (Option<MemoryWatchdog>, u64) {
    let rss_max = rss_max_from_env();
    let applied = install_self_data_limit(rss_max);
    let watchdog = MemoryWatchdog::start(rss_max);
    (watchdog, applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_reading_works_on_linux() {
        let rss = read_self_rss_bytes().expect("/proc/self/status is readable");
        assert!(rss > 0, "a live process has RSS");
    }

    #[test]
    fn a_one_byte_ceiling_trips_immediately() {
        let w = MemoryWatchdog::start(1).expect("1-byte ceiling is enabled");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while w.trip().is_none() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let trip = w.trip().expect("a 1-byte ceiling must trip");
        assert_eq!(trip.limit_bytes, 1);
        assert!(trip.rss_bytes > 1);
        assert!(w.diagnostic().unwrap().starts_with(HOST_MEMORY_LIMIT));
    }

    #[test]
    fn zero_ceiling_disables_the_watchdog() {
        assert!(MemoryWatchdog::start(0).is_none());
    }

    #[test]
    fn arming_a_process_group_records_it_as_killed() {
        // `arm_process_group` is what makes the trip actionable; the kill itself
        // is exercised by the integration spammer test (never on our own group).
        let w = MemoryWatchdog::start(1).unwrap();
        w.arm_process_group(0); // pid 0 would be our own group: must not be used here
        // Re-arm to a harmless, non-existent pid before the sampler fires.
        w.arm_process_group(0x7fff_fffe);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while w.trip().is_none() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(w.trip().is_some());
    }

    #[test]
    fn data_limit_defaults_above_the_rss_ceiling() {
        let base = 2 * 1024 * 1024 * 1024u64;
        let limit = data_limit_from_env(base);
        assert!(limit > base, "the rlimit backstop must leave room for the watchdog");
        assert_eq!(data_limit_from_env(0), 0);
    }

    #[test]
    fn relax_child_limits_is_safe_to_call() {
        // Must never panic or lower anything in the calling process.
        relax_child_limits();
    }

    #[test]
    fn the_stock_init_rc_nice_limit_is_mirrored_and_safe_to_apply() {
        // `init.rc` on the image: `setrlimit nice 40 40` (priorities 19..-20).
        assert_eq!(INIT_RLIMIT_NICE, 40);
        // Must never panic. On an unprivileged host (hard limit 0) the kernel
        // refuses the raise and the guest keeps the host limit; the gate below
        // must report that instead of claiming stock support.
        apply_init_rlimit_nice();
        let mut cur = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        let rc = unsafe { libc::getrlimit(libc::RLIMIT_NICE, &mut cur) };
        assert_eq!(rc, 0);
        assert_eq!(guest_priority_supported(), cur.rlim_max >= 24);
        note_guest_priority_gap("test");
    }
}
