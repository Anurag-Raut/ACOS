//! `android.os.SystemClock` — the boot-anchored clocks.
//!
//! All four uptime/elapsed variants come from one monotonic anchor taken when
//! the first clock call happens (the VM has no zygote, so "boot" is the first
//! clock read of the process). `elapsedRealtime` includes deep sleep on a
//! device; ACOS never suspends the guest clock, so it equals `uptime` and is
//! documented as such rather than invented.
//!
//! `currentThreadTimeMillis` is the calling OS thread's CPU time, read from
//! `/proc/thread-self/stat` (utime+stime). Linux reports those fields in
//! `USER_HZ` units, which is always 100 on Linux, so one tick is 10 ms.
//!
//! # Virtual time
//!
//! The lifecycle driver (`acos-runtime::lifecycle`) pumps the main looper to
//! quiescence. When every green thread is parked on a *future* message deadline
//! (a `Choreographer` `doFrame` at the next vsync, for example) the process is
//! idle and a real device would simply let the clock run. Blocking the host for
//! every such deadline is slow and non-deterministic, so the looper pump may
//! **advance** this clock to the earliest parked deadline instead of sleeping.
//! The guest sees exactly what it would see after sleeping that long: nothing is
//! faked, the monotonic timeline just moves forward.
//!
//! The offset is per host thread (the VM is single-threaded) and only ever
//! moves forward, so `uptime`/`elapsedRealtime` remain monotonic.

use std::cell::Cell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::vm::Vm;

/// `USER_HZ`: the unit of the utime/stime fields in `/proc/<pid>/stat`.
pub const CLOCK_TICKS_PER_SEC: u64 = 100;

thread_local! {
    /// Extra time the guest believes has elapsed, in nanoseconds. Advanced by
    /// the looper pump ([`advance_virtual_nanos`]) when the VM goes idle.
    static VIRTUAL_OFFSET_NANOS: Cell<i64> = const { Cell::new(0) };
}

fn boot() -> &'static Instant {
    static BOOT: OnceLock<Instant> = OnceLock::new();
    BOOT.get_or_init(Instant::now)
}

fn virtual_offset_nanos() -> i64 {
    VIRTUAL_OFFSET_NANOS.with(Cell::get)
}

fn uptime() -> std::time::Duration {
    let real = boot().elapsed();
    let offset = virtual_offset_nanos();
    if offset > 0 {
        real.saturating_add(Duration::from_nanos(offset as u64))
    } else {
        real
    }
}

/// Advance the guest monotonic clock by `nanos` nanoseconds.
///
/// Called by the looper pump when every green thread is parked on a future
/// deadline: the guest only observes a clock that has moved to that deadline,
/// exactly as if the host had slept until then. Only forward movement is
/// allowed so the timeline stays monotonic.
pub fn advance_virtual_nanos(nanos: i64) {
    if nanos <= 0 {
        return;
    }
    VIRTUAL_OFFSET_NANOS.with(|c| c.set(c.get().saturating_add(nanos)));
}

/// The current virtual offset in nanoseconds (0 until the first advance).
#[cfg(test)]
#[must_use]
pub fn virtual_offset() -> i64 {
    virtual_offset_nanos()
}

/// `SystemClock.uptimeMillis()` — milliseconds since boot, not counting sleep.
pub fn uptime_millis() -> i64 {
    uptime().as_millis() as i64
}

/// `SystemClock.uptimeNanos()` — nanoseconds since boot.
pub fn uptime_nanos() -> i64 {
    uptime().as_nanos() as i64
}

/// `SystemClock.elapsedRealtime()` — ACOS never suspends the clock, so this is
/// `uptimeMillis`.
pub fn elapsed_realtime_millis() -> i64 {
    uptime_millis()
}

/// `SystemClock.elapsedRealtimeNanos()`.
pub fn elapsed_realtime_nanos() -> i64 {
    uptime_nanos()
}

/// `SystemClock.currentTimeMicro()` — wall clock, microseconds since the epoch.
pub fn current_time_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// Parse utime+stime (in clock ticks) out of one `/proc/<pid>/stat` line.
///
/// The `comm` field is wrapped in parentheses and may itself contain spaces or
/// parentheses, so the fields after the **last** `)` are counted: index 0 is
/// field 3 (`state`), which makes `utime` (field 14) index 11 and `stime`
/// (field 15) index 12.
pub fn parse_thread_stat_ticks(stat: &str) -> Option<u64> {
    let close = stat.rfind(')')?;
    let fields: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    Some(utime + stime)
}

/// `SystemClock.currentThreadTimeMillis()` — CPU time of the calling thread.
/// Returns 0 when `/proc/thread-self` is unavailable (Linux < 3.17).
pub fn current_thread_time_millis() -> i64 {
    std::fs::read_to_string("/proc/thread-self/stat")
        .ok()
        .and_then(|s| parse_thread_stat_ticks(&s))
        .map(|ticks| (ticks * 1000 / CLOCK_TICKS_PER_SEC) as i64)
        .unwrap_or(0)
}

pub(crate) fn install(vm: &mut Vm) {
    let c = "Landroid/os/SystemClock;";
    vm.register_native(c, "uptimeMillis", "()J", uptime_millis_native);
    vm.register_native(c, "uptimeNanos", "()J", uptime_nanos_native);
    vm.register_native(c, "elapsedRealtime", "()J", elapsed_realtime_native);
    vm.register_native(
        c,
        "elapsedRealtimeNanos",
        "()J",
        elapsed_realtime_nanos_native,
    );
    vm.register_native(
        c,
        "currentThreadTimeMillis",
        "()J",
        current_thread_time_millis_native,
    );
    vm.register_native(c, "currentTimeMicro", "()J", current_time_micros_native);
}

fn uptime_millis_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, uptime_millis());
    Ok(())
}

fn uptime_nanos_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, uptime_nanos());
    Ok(())
}

fn elapsed_realtime_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, elapsed_realtime_millis());
    Ok(())
}

fn elapsed_realtime_nanos_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, elapsed_realtime_nanos());
    Ok(())
}

fn current_thread_time_millis_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, current_thread_time_millis());
    Ok(())
}

fn current_time_micros_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, current_time_micros());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn uptime_is_monotonic_and_boot_anchored() {
        let first = uptime_nanos();
        assert!(first > 0, "the boot anchor is taken before the first read");
        let second = uptime_nanos();
        assert!(second >= first);
        // Two reads are not the same instant, so only ordering is asserted.
        let ms = uptime_millis();
        let ns = uptime_nanos();
        assert!(ns / 1_000_000 >= ms, "millis and nanos share one anchor");
        assert_eq!(elapsed_realtime_millis(), uptime_millis());
        assert!(uptime_nanos() <= elapsed_realtime_nanos());
    }

    #[test]
    fn virtual_clock_advances_forward_only() {
        // The offset is per host thread, so this test thread starts at zero.
        assert_eq!(virtual_offset(), 0);
        advance_virtual_nanos(0);
        advance_virtual_nanos(-5);
        assert_eq!(virtual_offset(), 0, "only forward movement is allowed");
        let before = uptime_millis();
        advance_virtual_nanos(1_500_000_000);
        assert_eq!(virtual_offset(), 1_500_000_000);
        assert!(
            uptime_millis() >= before + 1_500,
            "uptime must observe the advanced offset"
        );
        assert_eq!(elapsed_realtime_millis(), uptime_millis());
    }

    #[test]
    fn uptime_advances_with_the_host_monotonic_clock() {
        let start = uptime_millis();
        std::thread::sleep(Duration::from_millis(20));
        let end = uptime_millis();
        assert!(
            end - start >= 15,
            "expected >=15 ms of uptime, got {}",
            end - start
        );
        assert!(
            end - start < 5_000,
            "unexpected clock jump: {}",
            end - start
        );
    }

    #[test]
    fn current_time_micros_tracks_the_wall_clock() {
        let host = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as i64;
        let vm_now = current_time_micros();
        assert!(
            (host - vm_now).abs() < 1_000_000,
            "currentTimeMicro {vm_now} vs host {host}"
        );
    }

    #[test]
    fn parses_proc_thread_stat_ticks_with_a_hostile_comm() {
        // comm = "a) b(c" (spaces and parentheses), utime=100, stime=50.
        let mut line = String::from("1234 (a) b(c) S");
        for field in 4..=52u32 {
            let value = match field {
                14 => "100",
                15 => "50",
                _ => "0",
            };
            line.push(' ');
            line.push_str(value);
        }
        assert_eq!(parse_thread_stat_ticks(&line), Some(150));
        assert_eq!(parse_thread_stat_ticks("garbage"), None);
        assert_eq!(parse_thread_stat_ticks("1 (x) S 1 2"), None);
    }

    #[test]
    fn current_thread_time_is_readable_on_this_host() {
        let first = current_thread_time_millis();
        assert!(first >= 0);
        // Burn a little CPU to make sure the counter is wired to this thread.
        let mut spin = 0u64;
        for i in 0..5_000_000u64 {
            spin = spin.wrapping_add(i ^ (spin >> 3));
        }
        assert_ne!(spin, 1);
        let second = current_thread_time_millis();
        assert!(
            second >= first,
            "thread CPU time went backwards: {first} -> {second}"
        );
        assert!(
            second < uptime_millis() + 60_000,
            "thread CPU time is implausible: {second}"
        );
    }
}
