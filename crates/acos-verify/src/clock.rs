//! Time sources for the runner.
//!
//! The runner never reads the clock directly; it asks a [`Clock`]. Production
//! uses [`SystemClock`]; tests use [`FixedClock`] so timestamps in the verdict and
//! frame metadata are deterministic and comparable.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A monotonic wall-clock source.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch (UTC).
    fn now_unix_ms(&self) -> u64;

    /// RFC 3339 UTC timestamp for the current instant.
    fn now_rfc3339(&self) -> String {
        rfc3339_from_unix_ms(self.now_unix_ms() as i64)
    }
}

/// The host wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// A clock that returns a fixed instant and only advances when [`FixedClock::advance_ms`]
/// is called. Used by tests for a reproducible verdict.
#[derive(Debug)]
pub struct FixedClock {
    millis: AtomicU64,
}

impl FixedClock {
    /// A clock frozen at `unix_ms`.
    pub fn new(unix_ms: u64) -> Self {
        FixedClock {
            millis: AtomicU64::new(unix_ms),
        }
    }

    /// Advance the clock by `delta_ms`.
    pub fn advance_ms(&self, delta_ms: u64) {
        self.millis.fetch_add(delta_ms, Ordering::Relaxed);
    }
}

impl Clock for FixedClock {
    fn now_unix_ms(&self) -> u64 {
        self.millis.load(Ordering::Relaxed)
    }
}

/// Format a Unix-epoch millisecond value as an RFC 3339 UTC string
/// (`2026-11-10T13:51:14.000Z`).
///
/// Implements the civil-from-days conversion (Howard Hinnant's algorithm) so the
/// crate does not need a date-time dependency.
pub fn rfc3339_from_unix_ms(unix_ms: i64) -> String {
    let millis_of_second = unix_ms.rem_euclid(1000);
    let seconds = unix_ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis_of_second:03}Z"
    )
}

/// Convert days since 1970-01-01 to a civil `(year, month, day)`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    ((if m <= 2 { y + 1 } else { y }), m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_formatted() {
        assert_eq!(rfc3339_from_unix_ms(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn known_instant_matches() {
        // 2026-11-10T13:51:14Z == 1794318674000 ms.
        assert_eq!(
            rfc3339_from_unix_ms(1_794_318_674_000),
            "2026-11-10T13:51:14.000Z"
        );
    }

    #[test]
    fn fixed_clock_advances() {
        let clock = FixedClock::new(1_794_318_674_000);
        assert_eq!(clock.now_rfc3339(), "2026-11-10T13:51:14.000Z");
        clock.advance_ms(250);
        assert_eq!(clock.now_rfc3339(), "2026-11-10T13:51:14.250Z");
    }
}
