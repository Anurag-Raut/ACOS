//! The time source and the "no wakeups when idle" instrumentation.
//!
//! The WMS/vsync/input state machines never call a timer syscall directly:
//! they ask a [`Clock`] to arm/disarm a deadline. [`VirtualClock`] records the
//! requested deadlines and counts arms, which is how the tests prove the
//! resting state is timer-free, and how [`crate::vsync`] runs deterministically.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

/// A monotonic clock plus a single outstanding timer.
///
/// "Arm" means "the runtime should wake me at `deadline_nanos`"; `disarm` means
/// "nothing is pending, the event loop may block forever". The seam exists so
/// the state machines are pure and testable.
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Monotonic nanoseconds (the value `System.nanoTime`/`event.timestamp` use).
    fn now_nanos(&self) -> i64;

    /// Arm the single pending deadline. Called only when a client asks for a
    /// vsync; never periodically.
    fn arm_timer(&self, deadline_nanos: i64);

    /// Cancel the pending deadline. Called when the last outstanding vsync
    /// request fires or is cancelled.
    fn disarm_timer(&self);

    /// `true` while a deadline is pending.
    fn timer_armed(&self) -> bool;

    /// Number of `arm_timer` calls since construction.
    fn arm_count(&self) -> u64;

    /// Number of `disarm_timer` calls since construction.
    fn disarm_count(&self) -> u64;
}

/// A deterministic virtual clock.
///
/// Time only moves when [`VirtualClock::advance`] is called, so a test can run
/// five simulated seconds of idle and assert `arm_count() == 0`.
#[derive(Debug)]
pub struct VirtualClock {
    now: Mutex<i64>,
    armed: Mutex<Option<i64>>,
    arms: AtomicU64,
    disarms: AtomicU64,
}

impl Default for VirtualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualClock {
    /// A clock anchored at `0` (boot).
    #[must_use]
    pub fn new() -> Self {
        Self {
            now: Mutex::new(0),
            armed: Mutex::new(None),
            arms: AtomicU64::new(0),
            disarms: AtomicU64::new(0),
        }
    }

    /// Move time forward by `nanos`.
    pub fn advance(&self, nanos: i64) {
        let mut now = self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *now += nanos.max(0);
    }

    /// The currently armed deadline, if any.
    #[must_use]
    pub fn armed_deadline(&self) -> Option<i64> {
        *self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reset the counters (used between bench phases).
    pub fn reset_counters(&self) {
        self.arms.store(0, Ordering::SeqCst);
        self.disarms.store(0, Ordering::SeqCst);
    }
}

impl Clock for VirtualClock {
    fn now_nanos(&self) -> i64 {
        *self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn arm_timer(&self, deadline_nanos: i64) {
        *self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(deadline_nanos);
        self.arms.fetch_add(1, Ordering::SeqCst);
    }

    fn disarm_timer(&self) {
        *self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.disarms.fetch_add(1, Ordering::SeqCst);
    }

    fn timer_armed(&self) -> bool {
        self.armed_deadline().is_some()
    }

    fn arm_count(&self) -> u64 {
        self.arms.load(Ordering::SeqCst)
    }

    fn disarm_count(&self) -> u64 {
        self.disarms.load(Ordering::SeqCst)
    }
}

/// A monotonic wall clock anchored at construction.
///
/// `now_nanos` is nanoseconds since the anchor, i.e. the same monotonic basis
/// `System.nanoTime` uses. The deadline fields are live only to keep the same
/// seam as [`VirtualClock`]; a real runtime arms an actual `timerfd` there.
#[derive(Debug)]
pub struct RealClock {
    anchor: Instant,
    armed: Mutex<Option<i64>>,
    arms: AtomicU64,
    disarms: AtomicU64,
}

impl Default for RealClock {
    fn default() -> Self {
        Self::new()
    }
}

impl RealClock {
    /// Anchor at the current instant.
    #[must_use]
    pub fn new() -> Self {
        Self {
            anchor: Instant::now(),
            armed: Mutex::new(None),
            arms: AtomicU64::new(0),
            disarms: AtomicU64::new(0),
        }
    }
}

impl Clock for RealClock {
    fn now_nanos(&self) -> i64 {
        self.anchor.elapsed().as_nanos() as i64
    }

    fn arm_timer(&self, deadline_nanos: i64) {
        *self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(deadline_nanos);
        self.arms.fetch_add(1, Ordering::SeqCst);
    }

    fn disarm_timer(&self) {
        *self
            .armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.disarms.fetch_add(1, Ordering::SeqCst);
    }

    fn timer_armed(&self) -> bool {
        self.armed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    fn arm_count(&self) -> u64 {
        self.arms.load(Ordering::SeqCst)
    }

    fn disarm_count(&self) -> u64 {
        self.disarms.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_clock_arms_and_disarms() {
        let c = VirtualClock::new();
        assert!(!c.timer_armed());
        c.advance(1_000);
        c.arm_timer(16_666_667);
        assert!(c.timer_armed());
        assert_eq!(c.armed_deadline(), Some(16_666_667));
        assert_eq!(c.arm_count(), 1);
        c.disarm_timer();
        assert!(!c.timer_armed());
        assert_eq!(c.disarm_count(), 1);
    }

    #[test]
    fn real_clock_is_monotonic() {
        let c = RealClock::new();
        let a = c.now_nanos();
        let b = c.now_nanos();
        assert!(b >= a);
    }
}
