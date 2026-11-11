//! `android.util.Log` — the log sink and its logcat line format.
//!
//! ACOS has no `logd`; `println_native` formats each entry exactly like
//! `logcat -v threadtime` and appends it to an in-process sink. The format is
//! pinned by `fixtures/reference/hello-views/art-api34-x86_64/logcat_initial.txt`:
//!
//! ```text
//! 11-10 13:51:14.009  7361  7361 I AcosFixture: onCreate
//! ```
//!
//!
//! That is `MM-DD HH:MM:SS.mmm`, pid (width 5), tid (width 5), the priority
//! letter, then `tag: msg`. Timestamps are UTC: the VM has no `/etc/localtime`
//! and a timezone would make runs non-reproducible.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime};

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::heap::NULL;
use crate::vm::Vm;

/// `android.util.Log.LOG_ID_MAIN`.
pub const LOG_ID_MAIN: i32 = 0;
/// `android.util.Log.VERBOSE`.
pub const PRIORITY_VERBOSE: i32 = 2;
/// `android.util.Log.DEBUG`.
pub const PRIORITY_DEBUG: i32 = 3;
/// `android.util.Log.INFO`.
pub const PRIORITY_INFO: i32 = 4;
/// `android.util.Log.WARN`.
pub const PRIORITY_WARN: i32 = 5;
/// `android.util.Log.ERROR`.
pub const PRIORITY_ERROR: i32 = 6;
/// `android.util.Log.ASSERT`.
pub const PRIORITY_ASSERT: i32 = 7;

/// `LOGGER_ENTRY_MAX_PAYLOAD` (`system/logging/liblog/include/log/log.h`).
pub const LOGGER_ENTRY_MAX_PAYLOAD: usize = 4068;

/// Ring-buffer size for the in-process sink.
///
/// logd keeps a 256 KiB per-buffer ring; 4096 records is the same order of
/// magnitude for ~64-byte lines and keeps a long run from growing without
/// bound. Oldest entries are dropped first, like logd.
pub const MAX_RECORDS: usize = 4096;

/// `isLoggable`'s documented default: INFO.
pub const DEFAULT_MIN_PRIORITY: i32 = PRIORITY_INFO;

/// One captured log entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRecord {
    /// `LOG_ID_*` buffer (`LOG_ID_MAIN = 0`, `LOG_ID_SYSTEM = 1`, …).
    pub buffer: i32,
    /// `Log.VERBOSE..ASSERT` (2..7).
    pub priority: i32,
    pub tag: String,
    pub msg: String,
    pub timestamp: SystemTime,
    pub pid: u32,
    pub tid: u32,
}

fn sink() -> &'static Mutex<VecDeque<LogRecord>> {
    static SINK: OnceLock<Mutex<VecDeque<LogRecord>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn lock() -> MutexGuard<'static, VecDeque<LogRecord>> {
    sink().lock().unwrap_or_else(|p| p.into_inner())
}

fn push(record: LogRecord) {
    let mut sink = lock();
    if sink.len() >= MAX_RECORDS {
        sink.pop_front();
    }
    sink.push_back(record);
}

/// Push one record into the log sink from host (non-guest) code.
///
/// Used by the platform services that must surface a structured event into the
/// same stream `acos logs` / `acos events` read (e.g. the toast the system
/// service was asked to show). The record is indistinguishable from a guest
/// `android.util.Log` write, which is the point: it is the platform logging, not
/// the app.
pub fn push_record(
    priority: i32,
    tag: &str,
    msg: &str,
    buffer: i32,
) {
    push(LogRecord {
        buffer,
        priority,
        tag: tag.to_string(),
        msg: truncate_payload(tag, msg),
        timestamp: SystemTime::now(),
        pid: std::process::id(),
        tid: current_tid(),
    });
}

/// Every record in the sink, oldest first.
pub fn records() -> Vec<LogRecord> {
    lock().iter().cloned().collect()
}

/// Remove and return every record, oldest first.
pub fn drain() -> Vec<LogRecord> {
    std::mem::take(&mut *lock()).into()
}

/// Remove and return the records written by one OS thread.
pub fn drain_thread(tid: u32) -> Vec<LogRecord> {
    let mut all = lock();
    let mut keep = VecDeque::with_capacity(all.len());
    let mut taken = Vec::new();
    while let Some(record) = all.pop_front() {
        if record.tid == tid {
            taken.push(record);
        } else {
            keep.push_back(record);
        }
    }
    *all = keep;
    taken
}

/// Remove and return the records written by this OS thread.
///
/// Tests use it so that concurrently running test functions do not steal each
/// other's entries; the VM runs on the caller's thread.
pub fn drain_current_thread() -> Vec<LogRecord> {
    drain_thread(current_tid())
}

/// The kernel thread id of the calling OS thread (`/proc/thread-self`), falling
/// back to the process id when procfs is unavailable.
pub fn current_tid() -> u32 {
    if let Ok(path) = std::fs::read_link("/proc/thread-self") {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if let Ok(tid) = name.parse::<u32>() {
                return tid;
            }
        }
    }
    std::process::id()
}

/// logcat's priority letter (`V D I W E A`), `?` for anything else.
pub fn priority_letter(priority: i32) -> char {
    match priority {
        PRIORITY_VERBOSE => 'V',
        PRIORITY_DEBUG => 'D',
        PRIORITY_INFO => 'I',
        PRIORITY_WARN => 'W',
        PRIORITY_ERROR => 'E',
        PRIORITY_ASSERT => 'A',
        _ => '?',
    }
}

/// Parse a `log.tag.*` value (`"D"`, `"info"`, `"4"`); `None` if it is not a
/// priority at all (which makes `isLoggable` fall through to the default).
pub fn parse_priority(value: &str) -> Option<i32> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(n) = value.parse::<i32>() {
        return (PRIORITY_VERBOSE..=PRIORITY_ASSERT).contains(&n).then_some(n);
    }
    match value.chars().next()?.to_ascii_uppercase() {
        'V' => Some(PRIORITY_VERBOSE),
        'D' => Some(PRIORITY_DEBUG),
        'I' => Some(PRIORITY_INFO),
        'W' => Some(PRIORITY_WARN),
        'E' => Some(PRIORITY_ERROR),
        'A' | 'F' | 'S' => Some(PRIORITY_ASSERT),
        _ => None,
    }
}

/// `Log.isLoggable`: the tag's `persist.log.tag.<tag>` / `log.tag.<tag>` level
/// (then the global `persist.log.tag` / `log.tag`), default INFO.
pub fn is_loggable(tag: &str, priority: i32) -> bool {
    // liblog limits the property lookup to the 23-character log tag.
    let tag: String = tag.chars().take(23).collect();
    let level = [
        format!("persist.log.tag.{tag}"),
        format!("log.tag.{tag}"),
        "persist.log.tag".to_string(),
        "log.tag".to_string(),
    ]
    .iter()
    .find_map(|key| crate::natives::os_props::get(key))
    .and_then(|v| parse_priority(&v))
    .unwrap_or(DEFAULT_MIN_PRIORITY);
    priority >= level
}

/// Truncate `msg` so that `prio + tag + msg` fits `LOGGER_ENTRY_MAX_PAYLOAD`,
/// on a UTF-8 boundary (liblog truncates the payload the same way).
pub fn truncate_payload(tag: &str, msg: &str) -> String {
    // One byte for the priority, one NUL after the tag.
    let overhead = tag.len() + 2;
    let budget = LOGGER_ENTRY_MAX_PAYLOAD.saturating_sub(overhead);
    if msg.len() <= budget {
        return msg.to_string();
    }
    let mut end = budget;
    while end > 0 && !msg.is_char_boundary(end) {
        end -= 1;
    }
    msg[..end].to_string()
}

/// The exact `logcat -v threadtime` line for one record.
pub fn threadtime_line(record: &LogRecord) -> String {
    let since_epoch = record
        .timestamp
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let days = (since_epoch.as_secs() / 86_400) as i64;
    let secs_of_day = since_epoch.as_secs() % 86_400;
    let (_, month, day) = civil_from_days(days);
    format_threadtime_parts(
        month,
        day,
        (secs_of_day / 3_600) as u32,
        ((secs_of_day % 3_600) / 60) as u32,
        (secs_of_day % 60) as u32,
        since_epoch.subsec_millis(),
        record.pid,
        record.tid,
        record.priority,
        &record.tag,
        &record.msg,
    )
}

/// Format one `logcat -v threadtime` line from its parts.
///
/// Split out from [`threadtime_line`] so the byte layout can be compared with
/// the ART reference capture without depending on the current time.
#[allow(clippy::too_many_arguments)]
pub fn format_threadtime_parts(
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    millis: u32,
    pid: u32,
    tid: u32,
    priority: i32,
    tag: &str,
    msg: &str,
) -> String {
    format!(
        "{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{millis:03} \
         {pid:>5} {tid:>5} {} {tag}: {msg}",
        priority_letter(priority)
    )
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub(crate) fn install(vm: &mut Vm) {
    let c = "Landroid/util/Log;";
    vm.register_native(
        c,
        "println_native",
        "(IILjava/lang/String;Ljava/lang/String;)I",
        println_native,
    );
    vm.register_native(
        c,
        "isLoggable",
        "(Ljava/lang/String;I)Z",
        is_loggable_native,
    );
    vm.register_native(
        c,
        "logger_entry_max_payload_native",
        "()I",
        logger_entry_max_payload_native,
    );
}

fn arg_string(vm: &Vm, ctx: NativeCtx, n: usize) -> String {
    let h = vm.arg_ref(ctx, n);
    if h == NULL {
        // liblog substitutes "" for a null tag/message.
        return String::new();
    }
    vm.string_to_rust(h).unwrap_or_default()
}

fn println_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let buffer = vm.arg_i32(ctx, 0);
    let priority = vm.arg_i32(ctx, 1);
    let tag = arg_string(vm, ctx, 2);
    let msg = truncate_payload(&tag, &arg_string(vm, ctx, 3));
    // `__android_log_write` returns the number of bytes it wrote; the value is
    // ignored by `Log.*`. ACOS reports the message bytes the sink accepted and
    // never -1 (the sink cannot fail).
    let written = msg.len() as i32;
    push(LogRecord {
        buffer,
        priority,
        tag,
        msg,
        timestamp: SystemTime::now(),
        pid: std::process::id(),
        tid: current_tid(),
    });
    vm.ret_i32(ctx, written);
    Ok(())
}

fn is_loggable_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let tag = arg_string(vm, ctx, 0);
    let priority = vm.arg_i32(ctx, 1);
    vm.ret_bool(ctx, is_loggable(&tag, priority));
    Ok(())
}

fn logger_entry_max_payload_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, LOGGER_ENTRY_MAX_PAYLOAD as i32);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The sink is process-wide; tests that read it must not interleave.
    fn sink_test_guard() -> MutexGuard<'static, ()> {
        static GUARD: Mutex<()> = Mutex::new(());
        GUARD.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn reference_logcat() -> Option<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path = root.join("fixtures/reference/hello-views/art-api34-x86_64/logcat_initial.txt");
        std::fs::read_to_string(path).ok()
    }

    #[test]
    fn threadtime_layout_matches_the_art_reference_capture() {
        let Some(text) = reference_logcat() else {
            eprintln!("SKIP threadtime_layout_matches_the_art_reference_capture: no capture");
            return;
        };
        let reference = text
            .lines()
            .find(|l| l.contains("AcosFixture: onCreate"))
            .expect("the reference capture has the onCreate line");
        // The reference is a real `logcat -v threadtime` line. Reproduce that
        // exact byte layout for the same fields.
        let expected = "11-10 13:51:14.009  7361  7361 I AcosFixture: onCreate";
        assert_eq!(reference, expected, "fixture/logcat format changed");
        let ours = format_threadtime_parts(
            11,
            10,
            13,
            51,
            14,
            9,
            7361,
            7361,
            PRIORITY_INFO,
            "AcosFixture",
            "onCreate",
        );
        assert_eq!(ours, reference);
    }

    #[test]
    fn priority_letters_cover_the_log_levels() {
        assert_eq!(priority_letter(PRIORITY_VERBOSE), 'V');
        assert_eq!(priority_letter(PRIORITY_DEBUG), 'D');
        assert_eq!(priority_letter(PRIORITY_INFO), 'I');
        assert_eq!(priority_letter(PRIORITY_WARN), 'W');
        assert_eq!(priority_letter(PRIORITY_ERROR), 'E');
        assert_eq!(priority_letter(PRIORITY_ASSERT), 'A');
        assert_eq!(priority_letter(0), '?');
    }

    #[test]
    fn parse_priority_accepts_letters_and_numbers() {
        assert_eq!(parse_priority("V"), Some(PRIORITY_VERBOSE));
        assert_eq!(parse_priority("d"), Some(PRIORITY_DEBUG));
        assert_eq!(parse_priority("info"), Some(PRIORITY_INFO));
        assert_eq!(parse_priority("4"), Some(PRIORITY_INFO));
        assert_eq!(parse_priority("  W "), Some(PRIORITY_WARN));
        assert_eq!(parse_priority("1"), None);
        assert_eq!(parse_priority(""), None);
        assert_eq!(parse_priority("verbose"), Some(PRIORITY_VERBOSE));
    }

    #[test]
    fn dates_render_in_utc_with_millisecond_padding() {
        let line = |secs: u64, millis: u32| {
            let record = LogRecord {
                buffer: LOG_ID_MAIN,
                priority: PRIORITY_DEBUG,
                tag: "T".to_string(),
                msg: "m".to_string(),
                timestamp: SystemTime::UNIX_EPOCH + Duration::new(secs, millis * 1_000_000),
                pid: 1,
                tid: 2,
            };
            threadtime_line(&record)
        };
        assert!(line(0, 0).starts_with("01-01 00:00:00.000 "));
        assert!(line(86_400, 0).starts_with("01-02 00:00:00.000 "));
        // 2000-01-01 and 2024-02-29 (leap day) as day counts since the epoch.
        assert!(line(10_957 * 86_400, 5).starts_with("01-01 00:00:00.005 "));
        assert!(line(19_782 * 86_400, 999).starts_with("02-29 00:00:00.999 "));
        assert!(line(20_402 * 86_400 + 13 * 3_600 + 51 * 60 + 14, 9)
            .starts_with("11-10 13:51:14.009 "));
        assert_eq!(
            line(0, 0),
            "01-01 00:00:00.000     1     2 D T: m",
            "pid/tid are right-aligned in 5 columns"
        );
    }

    #[test]
    fn sink_is_per_thread_and_drains() {
        let _guard = sink_test_guard();
        let tid = current_tid();
        let before = drain_current_thread();
        assert!(before.is_empty() || before.iter().all(|r| r.tid == tid));
        let pid = std::process::id();
        push(LogRecord {
            buffer: LOG_ID_MAIN,
            priority: PRIORITY_INFO,
            tag: "AcosTest".to_string(),
            msg: "hello".to_string(),
            timestamp: SystemTime::now(),
            pid,
            tid,
        });
        let taken = drain_current_thread();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].tag, "AcosTest");
        assert_eq!(taken[0].msg, "hello");
        assert_eq!(taken[0].pid, pid);
        assert_eq!(taken[0].tid, tid);
        assert!(drain_current_thread().is_empty(), "drain clears the sink");
        assert!(!threadtime_line(&taken[0]).is_empty());
    }

    #[test]
    fn is_loggable_defaults_to_info() {
        assert!(!is_loggable("AcosLogTest", PRIORITY_VERBOSE));
        assert!(!is_loggable("AcosLogTest", PRIORITY_DEBUG));
        assert!(is_loggable("AcosLogTest", PRIORITY_INFO));
        assert!(is_loggable("AcosLogTest", PRIORITY_ERROR));
        // A tag-specific override wins over the INFO default.
        assert!(crate::natives::os_props::set("log.tag.AcosLogTest", "E"));
        assert!(!is_loggable("AcosLogTest", PRIORITY_WARN));
        assert!(is_loggable("AcosLogTest", PRIORITY_ERROR));
        assert!(crate::natives::os_props::unset("log.tag.AcosLogTest"));
        assert!(is_loggable("AcosLogTest", PRIORITY_INFO));
        // Tags longer than 23 characters use the 23-character prefix.
        let long = "A".repeat(30);
        assert!(is_loggable(&long, PRIORITY_INFO));
    }

    #[test]
    fn sink_is_a_ring_buffer() {
        let _guard = sink_test_guard();
        let _ = drain();
        let tid = current_tid();
        for i in 0..(MAX_RECORDS + 10) {
            push(LogRecord {
                buffer: LOG_ID_MAIN,
                priority: PRIORITY_DEBUG,
                tag: "AcosRing".to_string(),
                msg: i.to_string(),
                timestamp: SystemTime::now(),
                pid: std::process::id(),
                tid,
            });
        }
        let all = records();
        assert_eq!(all.len(), MAX_RECORDS);
        assert_eq!(all[0].msg, "10", "the oldest entries are dropped");
        assert_eq!(all[MAX_RECORDS - 1].msg, (MAX_RECORDS + 9).to_string());
        let _ = drain();
    }

    #[test]
    fn payload_is_truncated_to_the_logger_limit() {
        let tag = "AcosFixture";
        assert_eq!(truncate_payload(tag, "short"), "short");
        let long = "x".repeat(LOGGER_ENTRY_MAX_PAYLOAD * 2);
        let cut = truncate_payload(tag, &long);
        assert!(cut.len() + tag.len() + 2 <= LOGGER_ENTRY_MAX_PAYLOAD);
        assert_eq!(cut.len(), LOGGER_ENTRY_MAX_PAYLOAD - tag.len() - 2);
        // Multi-byte characters are cut on a char boundary.
        let utf8 = "é".repeat(LOGGER_ENTRY_MAX_PAYLOAD);
        let cut = truncate_payload("t", &utf8);
        assert!(cut.len() <= LOGGER_ENTRY_MAX_PAYLOAD - 3);
        assert!(std::str::from_utf8(cut.as_bytes()).is_ok());
    }
}
