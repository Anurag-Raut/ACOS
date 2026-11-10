//! Host-side resource ceilings for a VM run.
//!
//! The guest heap limit (`ACOS_HEAP_MAX` / `Heap::max_bytes`) only bounds
//! *managed* bytes. A VM bug, a host-side cache that is never pruned, or a
//! scheduler spin that keeps allocating can still drive the **host** process to
//! tens of gigabytes and take the machine down (host-safety incident: a debug
//! `acos run-dex` reached 45 GB RSS on a 62 GB, no-swap host).
//!
//! [`HostGuard`] adds two hard, cheap ceilings that are independent of the guest
//! heap:
//!
//! * `ACOS_HOST_RSS_MAX` (bytes, default 2 GiB) — sampled from
//!   `/proc/self/status:VmRSS` at most every 100 ms on allocation and scheduler
//!   iterations. Crossing it aborts with [`VmError::HostMemoryLimit`]
//!   (`HOST_MEMORY_LIMIT`).
//! * `ACOS_WALL_MAX_SECS` (default 600) — a wall-clock watchdog for a hung
//!   interpreter, scheduler spin or blocking looper. Crossing it aborts with
//!   [`VmError::Timeout`] (`TIMEOUT`).
//!
//! Both accept `0` to disable. A suffix (`K`/`M`/`G`, case-insensitive) is
//! accepted for the byte value, e.g. `ACOS_HOST_RSS_MAX=512M`.

use std::time::{Duration, Instant};

use crate::error::{VmError, VmResult};

/// Default host RSS ceiling: 2 GiB.
pub const DEFAULT_HOST_RSS_MAX: usize = 2 * 1024 * 1024 * 1024;

/// Default wall-clock ceiling: 10 minutes.
pub const DEFAULT_WALL_MAX_SECS: u64 = 600;

/// Minimum interval between `/proc` reads. The wall check is free; the RSS
/// read is throttled so the hot allocation/scheduler paths stay cheap.
const RSS_CHECK_INTERVAL: Duration = Duration::from_millis(100);

/// A hard host-side memory and wall-clock ceiling.
#[derive(Debug)]
pub struct HostGuard {
    rss_max_bytes: usize,
    wall_max: Option<Duration>,
    started: Instant,
    last_rss_check: Instant,
    /// Last observed RSS (bytes), for structured errors and metrics.
    rss_now: usize,
    /// Call counter used to amortise the two `Instant::now` reads below. The
    /// allocation hot path calls `check` once per object; reading the clock
    /// twice per allocation measurably dominated allocator-bound programs.
    tick: u32,
}

impl HostGuard {
    /// Build from `ACOS_HOST_RSS_MAX` / `ACOS_WALL_MAX_SECS` (with defaults).
    pub fn from_env() -> HostGuard {
        let rss_max_bytes = std::env::var("ACOS_HOST_RSS_MAX")
            .ok()
            .and_then(|s| parse_bytes(&s))
            .unwrap_or(DEFAULT_HOST_RSS_MAX);
        let wall_max_secs = std::env::var("ACOS_WALL_MAX_SECS")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_WALL_MAX_SECS);
        HostGuard::new(rss_max_bytes, wall_max_secs)
    }

    /// Build explicitly. `rss_max_bytes == 0` disables the memory ceiling;
    /// `wall_max_secs == 0` disables the watchdog.
    pub fn new(rss_max_bytes: usize, wall_max_secs: u64) -> HostGuard {
        let now = Instant::now();
        HostGuard {
            rss_max_bytes,
            wall_max: (wall_max_secs != 0).then(|| Duration::from_secs(wall_max_secs)),
            started: now,
            last_rss_check: now,
            rss_now: 0,
            tick: 0,
        }
    }

    /// Re-arm the wall clock at the start of a `run` (boot parsing before the
    /// first `run` is not counted). Keeps the memory ceiling live.
    pub fn arm_wall(&mut self) {
        self.started = Instant::now();
    }

    /// Cheap check. Tests the wall clock at most every 64 calls and reads
    /// `/proc/self/status` at most every [`RSS_CHECK_INTERVAL`]. Amortising the
    /// clock reads keeps the per-allocation cost to an integer increment while
    /// bounding watchdog detection latency to a few microseconds of VM work.
    pub fn check(&mut self) -> VmResult<()> {
        // The wall clock is a *hard* ceiling: evaluate it on every call so a
        // loop that allocates cannot run up to 64 allocations past the limit.
        // (`Instant::now` is the cheap part; the `/proc` read is what is
        // throttled below.)
        if let Some(max) = self.wall_max {
            let elapsed = self.started.elapsed();
            if elapsed > max {
                return Err(VmError::Timeout {
                    elapsed_secs: elapsed.as_secs(),
                    limit_secs: max.as_secs(),
                });
            }
        }
        self.tick = self.tick.wrapping_add(1);
        // Always do the first RSS sample (so `rss_now` is populated), then
        // sample once per 64 calls.
        if self.rss_now != 0 && self.tick & 0x3f != 0 {
            return Ok(());
        }
        if self.rss_max_bytes != 0 {
            let now = Instant::now();
            if now.duration_since(self.last_rss_check) >= RSS_CHECK_INTERVAL || self.rss_now == 0 {
                self.last_rss_check = now;
                if let Some(rss) = read_rss_bytes() {
                    self.rss_now = rss;
                    if rss > self.rss_max_bytes {
                        return Err(VmError::HostMemoryLimit {
                            rss_bytes: rss,
                            limit_bytes: self.rss_max_bytes,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Time left before the wall watchdog fires, if it is armed. `Some(ZERO)`
    /// means already expired; `None` means disabled.
    pub fn remaining_wall(&self) -> Option<Duration> {
        self.wall_max
            .map(|m| m.saturating_sub(self.started.elapsed()))
    }

    /// Last observed RSS in bytes (`0` until the first sample).
    pub fn rss_bytes(&self) -> usize {
        self.rss_now
    }

    /// Configured RSS ceiling in bytes (`0` = disabled).
    pub fn rss_max_bytes(&self) -> usize {
        self.rss_max_bytes
    }
}

impl Default for HostGuard {
    fn default() -> Self {
        HostGuard::from_env()
    }
}

/// Read the host process RSS from `/proc/self/status:VmRSS` (kB).
fn read_rss_bytes() -> Option<usize> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: usize = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

/// Parse a byte count with an optional `K`/`M`/`G` suffix.
pub fn parse_bytes(s: &str) -> Option<usize> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, mult) = match s.chars().last()?.to_ascii_uppercase() {
        'K' | 'k' => (&s[..s.len() - 1], 1024usize),
        'M' | 'm' => (&s[..s.len() - 1], 1024 * 1024),
        'G' | 'g' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    num.trim()
        .parse::<usize>()
        .ok()
        .map(|n| n.saturating_mul(mult))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bytes_accepts_suffixes_and_plain() {
        assert_eq!(parse_bytes("1024"), Some(1024));
        assert_eq!(parse_bytes("2K"), Some(2048));
        assert_eq!(parse_bytes("3m"), Some(3 * 1024 * 1024));
        assert_eq!(parse_bytes("1G"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_bytes(" 512M "), Some(512 * 1024 * 1024));
        assert_eq!(parse_bytes("nope"), None);
    }

    #[test]
    fn zero_rss_budget_reports_host_memory_limit() {
        // A ceiling of one byte is below any live process RSS; the first check
        // must abort with the structured code.
        let mut guard = HostGuard::new(1, 0);
        let err = guard.check().expect_err("1-byte ceiling must trip");
        assert_eq!(err.code(), "HOST_MEMORY_LIMIT");
    }

    #[test]
    fn expired_wall_budget_reports_timeout() {
        let mut guard = HostGuard::new(0, 0);
        // Force an already-elapsed window without sleeping.
        guard.wall_max = Some(Duration::from_nanos(1));
        guard.started = Instant::now() - Duration::from_secs(1);
        let err = guard.check().expect_err("elapsed wall budget must trip");
        assert_eq!(err.code(), "TIMEOUT");
    }

    #[test]
    fn disabled_guard_never_trips() {
        let mut guard = HostGuard::new(0, 0);
        assert!(guard.check().is_ok());
    }
}
