//! Native-crash evidence for a guest that died of a signal (R3).
//!
//! When an ART guest is killed by SIGSEGV/SIGBUS/SIGILL/SIGFPE/SIGABRT the
//! launcher used to report only `guest_signal` and the stage: the fault
//! address, the `si_code`, the faulting thread and the stack were lost with the
//! process. The guest now prints, from its own signal handler:
//!
//! 1. one machine-readable line
//!    `ACOSCRASH {"source":"acos-shim","signal":11,"code":1,"fault_addr":"0x0",…}`
//!    (always available: raw syscalls, no allocation), and
//! 2. ART's own crash dump on stderr — the block a device gets from debuggerd,
//!    produced by `art::HandleUnexpectedSignalCommon`:
//!
//! ```text
//! *** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
//! Fatal signal 11 (SIGSEGV), code 1 (SEGV_MAPERR) fault addr 0x0
//! OS: Linux 6.8.0 …
//! Cmdline: /system/bin/app_process64 …
//! Thread: 1234 "mqt_js"
//! Registers:
//! …
//! Backtrace:
//!   #00 pc 00000000007c1f34  /data/app-lib/x86_64/libhermesvm.so (offset 0x…)
//! ```
//!
//! This module turns both into one structured record for
//! `session-exit.json` (`native_crash`) and keeps the raw block next to it
//! (`native-crash.txt`) so nothing is lost if the parser meets a format it does
//! not know. It never fabricates a field: every value comes from the guest's
//! own output, and a missing piece is simply absent.

use serde_json::{json, Value};

use crate::LogEntry;

/// Marker the guest's handler prints (and mirrors into the logd sink).
pub const CRASH_MARKER: &str = "ACOSCRASH ";

/// The first line of ART's own unexpected-signal dump.
const ART_HEADER: &str = "*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***";

/// Cap on the retained raw dump; a stack-overflow dump of every thread can be
/// large, and the first blocks are the ones that identify the fault.
pub const RAW_CAP: usize = 64 * 1024;

/// Cap on parsed frames per crash (index -1 is the faulting pc).
const MAX_FRAMES: usize = 48;

/// A parsed native crash: the structured fields an agent needs to name the
/// library and thread that faulted.
#[derive(Debug, Clone, Default)]
pub struct NativeCrash {
    /// Raw `ACOSCRASH` JSON as the guest printed it.
    pub shim: Option<Value>,
    /// The ART dump text (header through the end of the backtrace).
    pub raw: String,
    pub signal: Option<i64>,
    pub signal_name: Option<String>,
    pub code: Option<i64>,
    pub code_name: Option<String>,
    pub fault_addr: Option<String>,
    pub fault_pc: Option<String>,
    pub thread_tid: Option<i64>,
    pub thread_name: Option<String>,
    pub cmdline: Option<String>,
    pub frames: Vec<Frame>,
    /// Registered frames that are not from a mapped file (JIT/anonymous), kept
    /// so an "unknown" frame is visibly unknown rather than dropped.
    pub unmapped_frames: usize,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub index: usize,
    pub pc: String,
    pub map: Option<String>,
    pub offset: Option<String>,
    pub symbol: Option<String>,
}

impl NativeCrash {
    /// The single most useful sentence: which library and which thread.
    pub fn summary(&self) -> String {
        let sig = match (&self.signal_name, self.signal) {
            (Some(n), Some(v)) => format!("{n} ({v})"),
            (Some(n), None) => n.clone(),
            (None, Some(v)) => format!("signal {v}"),
            _ => "unknown signal".to_string(),
        };
        let thread = match (&self.thread_name, self.thread_tid) {
            (Some(n), Some(t)) => format!("thread {t} \"{n}\""),
            (Some(n), None) => format!("thread \"{n}\""),
            (None, Some(t)) => format!("tid {t}"),
            _ => "unknown thread".to_string(),
        };
        let where_ = match self.frames.first() {
            Some(f) => match (&f.map, &f.symbol, &f.offset) {
                (Some(m), Some(s), _) => format!("{m} ({s})"),
                (Some(m), None, Some(o)) => format!("{m} at {o}"),
                (Some(m), None, None) => m.clone(),
                _ => format!("pc {}", f.pc),
            },
            None => "no frame".to_string(),
        };
        let addr = self
            .fault_addr
            .as_deref()
            .map(|a| format!(" fault_addr={a}"))
            .unwrap_or_default();
        format!("{sig}{addr} in {thread}: {where_}")
    }

    pub fn to_json(&self) -> Value {
        json!({
            "signal": self.signal,
            "signal_name": self.signal_name,
            "code": self.code,
            "code_name": self.code_name,
            "fault_addr": self.fault_addr,
            "fault_pc": self.fault_pc,
            "thread": {
                "tid": self.thread_tid,
                "name": self.thread_name,
            },
            "cmdline": self.cmdline,
            "summary": self.summary(),
            "frames": self.frames.iter().map(|f| json!({
                "index": f.index,
                "pc": f.pc,
                "map": f.map,
                "offset": f.offset,
                "symbol": f.symbol,
            })).collect::<Vec<_>>(),
            "unmapped_frames": self.unmapped_frames,
            "shim": self.shim,
        })
    }
}

/// Split `Fatal signal 11 (SIGSEGV), code 1 (SEGV_MAPERR) fault addr 0x0`.
fn parse_fatal(
    line: &str,
) -> (
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<String>,
) {
    let mut signal = None;
    let mut signal_name = None;
    let mut code = None;
    let mut code_name = None;
    let mut fault_addr = None;
    let rest = match line.strip_prefix("Fatal signal ") {
        Some(r) => r,
        None => return (signal, signal_name, code, code_name, fault_addr),
    };
    // "11 (SIGSEGV), code 1 (SEGV_MAPERR) fault addr 0x0"
    let (num, tail) = match rest.split_once(' ') {
        Some((n, t)) => (n, t),
        None => (rest, ""),
    };
    signal = num.trim().parse::<i64>().ok();
    if let Some(open) = tail.find('(') {
        if let Some(close) = tail[open..].find(')') {
            signal_name = Some(tail[open + 1..open + close].to_string());
        }
    }
    if let Some(pos) = tail.find("code ") {
        let after = &tail[pos + 5..];
        let num: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        code = num.parse::<i64>().ok();
        if let Some(open) = after.find('(') {
            if let Some(close) = after[open..].find(')') {
                code_name = Some(after[open + 1..open + close].to_string());
            }
        }
    }
    if let Some(pos) = tail.find("fault addr ") {
        let after = &tail[pos + 11..];
        let addr: String = after
            .chars()
            .take_while(|c| c.is_ascii_hexdigit() || *c == 'x' || *c == 'X')
            .collect();
        if !addr.is_empty() {
            fault_addr = Some(addr);
        }
    }
    (signal, signal_name, code, code_name, fault_addr)
}

/// Parse one `Backtrace:` frame line.
///
/// ART prints `#%02zu pc <hex>  <map> (<symbol>+<off>)` or
/// `#%02zu pc <hex>  <map> offset <hex>`; a frame with no mapped file prints
/// the pc alone. The parser keeps whatever it finds and never demands a shape.
fn parse_frame(line: &str) -> Option<Frame> {
    let t = line.trim_start();
    let t = t.strip_prefix('#')?;
    let (idx, rest) = t.split_once(' ')?;
    let index: usize = idx.trim().parse().ok()?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("pc ")?.trim_start();
    let mut parts = rest.split_whitespace();
    let pc = parts.next()?.to_string();
    if !pc.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let remainder = rest[pc.len()..].trim();
    let mut map = None;
    let mut offset = None;
    let mut symbol = None;
    if !remainder.is_empty() {
        // The map is the first token that looks like a path or a file name.
        for token in remainder.split_whitespace() {
            let tok = token.trim_matches(|c| c == '(' || c == ')');
            if tok.starts_with('/')
                || tok.ends_with(".so")
                || tok == "(deleted)"
                || tok.contains(".so")
            {
                map = Some(tok.to_string());
                break;
            }
        }
        if let Some(pos) = remainder.find("offset ") {
            let val: String = remainder[pos + 7..]
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == 'x')
                .collect();
            if !val.is_empty() {
                offset = Some(val);
            }
        }
        if let Some(open) = remainder.rfind('(') {
            if let Some(close) = remainder[open..].find(')') {
                let inner = &remainder[open + 1..open + close];
                if !inner.is_empty() && !inner.starts_with("offset") {
                    symbol = Some(inner.to_string());
                }
            }
        }
    }
    Some(Frame {
        index,
        pc,
        map,
        offset,
        symbol,
    })
}

/// Extract the ART dump block (header through the last backtrace frame) from a
/// stream. Tolerant: a truncated dump yields the lines that exist.
pub fn art_block(text: &str) -> Option<String> {
    let start = text.find(ART_HEADER)?;
    let tail = &text[start..];
    let mut out = String::new();
    let mut in_frames = false;
    let mut frames = 0usize;
    for line in tail.lines() {
        let trimmed = line.trim_start();
        if in_frames {
            if trimmed.starts_with('#') && parse_frame(trimmed).is_some() {
                frames += 1;
                out.push_str(line);
                out.push('\n');
                if frames >= MAX_FRAMES {
                    break;
                }
                continue;
            }
            if trimmed.is_empty() {
                continue;
            }
            // The backtrace ended (a new section, a "Fault message:", the next
            // thread header, or the guest's own marker line).
            if trimmed.starts_with("***") || trimmed.starts_with("Fault message:") {
                break;
            }
            break;
        }
        out.push_str(line);
        out.push('\n');
        if trimmed.starts_with("Backtrace:") {
            in_frames = true;
        }
        if out.len() >= RAW_CAP {
            break;
        }
    }
    let out = out.trim_end().to_string();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Build the crash record from the guest's streams and logd capture.
///
/// `stdout`/`stderr` are the launcher's bounded tails (multi-MiB by default),
/// `logs` the logd sink's entries. Returns `None` when the guest produced
/// neither an `ACOSCRASH` line nor an ART dump — the caller then keeps its
/// plain `guest_signal` evidence instead of inventing a record.
pub fn analyze(stdout: &str, stderr: &str, logs: &[LogEntry]) -> Option<NativeCrash> {
    let mut crash = NativeCrash::default();
    'shim: for text in [stderr, stdout] {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix(CRASH_MARKER) {
                if let Ok(v) = serde_json::from_str::<Value>(rest) {
                    crash.shim = Some(v);
                    break 'shim;
                }
            }
        }
    }
    if crash.shim.is_none() {
        for e in logs.iter().rev() {
            if let Some(rest) = e.message.strip_prefix(CRASH_MARKER) {
                if let Ok(v) = serde_json::from_str::<Value>(rest) {
                    crash.shim = Some(v);
                    break;
                }
            }
        }
    }
    // ART's dump: stderr first (dump_on_stderr=1 writes to fd 2), then logd for
    // a guest whose handler was reached through a log-only path.
    crash.raw = art_block(stderr)
        .or_else(|| {
            let joined: Vec<String> = logs.iter().map(|e| e.message.clone()).collect();
            art_block(&joined.join("\n"))
        })
        .unwrap_or_default();

    if let Some(shim) = &crash.shim {
        crash.signal = shim.get("signal").and_then(Value::as_i64);
        crash.code = shim.get("code").and_then(Value::as_i64);
        crash.fault_addr = shim
            .get("fault_addr")
            .and_then(Value::as_str)
            .map(str::to_string);
        crash.thread_tid = shim.get("tid").and_then(Value::as_i64);
        crash.thread_name = shim
            .get("thread")
            .and_then(Value::as_str)
            .map(str::to_string);
    }

    let mut in_frames = false;
    for line in crash.raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Fatal signal ") {
            let (sig, sig_name, code, code_name, addr) = parse_fatal(trimmed);
            crash.signal = crash.signal.or(sig);
            crash.signal_name = sig_name;
            crash.code = crash.code.or(code);
            crash.code_name = code_name;
            if crash.fault_addr.is_none() {
                crash.fault_addr = addr;
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Thread: ") {
            // `1234 "name"`
            let (tid, name) = match rest.split_once(' ') {
                Some((t, n)) => (t.trim().parse::<i64>().ok(), Some(n.trim())),
                None => (rest.trim().parse::<i64>().ok(), None),
            };
            crash.thread_tid = crash.thread_tid.or(tid);
            if crash.thread_name.is_none() {
                crash.thread_name = name.map(|n| n.trim_matches('"').to_string());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Cmdline: ") {
            crash.cmdline = Some(rest.to_string());
            continue;
        }
        if trimmed == "Backtrace:" {
            in_frames = true;
            continue;
        }
        if in_frames {
            match parse_frame(trimmed) {
                Some(f) => {
                    if f.index == 0 {
                        crash.fault_pc = Some(f.pc.clone());
                    }
                    if f.map.is_none() {
                        crash.unmapped_frames += 1;
                    }
                    crash.frames.push(f);
                }
                None => in_frames = false,
            }
        }
    }
    if crash.signal_name.is_none() {
        crash.signal_name = crash.signal.and_then(signal_name).map(str::to_string);
    }
    if crash.shim.is_none() && crash.raw.is_empty() {
        return None;
    }
    Some(crash)
}

/// Signal names for the case where only the shim's line exists (ART's own
/// `GetSignalCodeName` did not run). Same spelling ART/debuggerd use.
#[must_use]
pub fn signal_name(signal: i64) -> Option<&'static str> {
    match signal {
        11 => Some("SIGSEGV"),
        7 => Some("SIGBUS"),
        4 => Some("SIGILL"),
        8 => Some("SIGFPE"),
        6 => Some("SIGABRT"),
        5 => Some("SIGTRAP"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "\
*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
Fatal signal 11 (SIGSEGV), code 1 (SEGV_MAPERR) fault addr 0x0
OS: Linux 6.8.0-45-generic #45~22.04.1-Ubuntu SMP x86_64
Cmdline: /system/bin/app_process64
Thread: 4412 \"mqt_v_native\"
Registers:
  rax 0000000000000000  rbx 0000000000000001
Backtrace:
      #00 pc 00000000007c1f34  /data/app-lib/x86_64/libhermesvm.so
      #01 pc 00000000000a1b20  /data/app-lib/x86_64/libfbjni.so
      #02 pc 0000000000000000  <unknown>
";

    #[test]
    fn art_dump_yields_signal_code_addr_thread_and_library_frames() {
        let c = analyze("", DUMP, &[]).expect("a dump is a crash record");
        assert_eq!(c.signal, Some(11));
        assert_eq!(c.signal_name.as_deref(), Some("SIGSEGV"));
        assert_eq!(c.code, Some(1));
        assert_eq!(c.code_name.as_deref(), Some("SEGV_MAPERR"));
        assert_eq!(c.fault_addr.as_deref(), Some("0x0"));
        assert_eq!(c.thread_tid, Some(4412));
        assert_eq!(c.thread_name.as_deref(), Some("mqt_v_native"));
        assert_eq!(c.fault_pc.as_deref(), Some("00000000007c1f34"));
        assert_eq!(c.frames.len(), 3);
        assert_eq!(
            c.frames[0].map.as_deref(),
            Some("/data/app-lib/x86_64/libhermesvm.so")
        );
        assert_eq!(c.unmapped_frames, 1);
        assert!(c.summary().contains("libhermesvm.so"), "{}", c.summary());
        assert!(c.summary().contains("mqt_v_native"), "{}", c.summary());
    }

    #[test]
    fn shim_line_alone_is_enough_for_the_primary_fields() {
        let line = r#"ACOSCRASH {"source":"acos-shim","signal":11,"code":1,"fault_addr":"0xacc0dead","pid":42,"tid":43,"thread":"acos-crash-test"}"#;
        let c = analyze("", line, &[]).expect("the shim line is evidence");
        assert_eq!(c.signal, Some(11));
        assert_eq!(c.fault_addr.as_deref(), Some("0xacc0dead"));
        assert_eq!(c.thread_name.as_deref(), Some("acos-crash-test"));
        assert!(c.raw.is_empty());
        // No ART dump: the record still says which thread faulted and where.
        assert!(c.summary().contains("acos-crash-test"));
    }

    #[test]
    fn nothing_is_not_a_crash_record() {
        assert!(analyze("all good\n", "no fault here\n", &[]).is_none());
    }

    #[test]
    fn truncated_dump_keeps_what_exists() {
        let truncated = "\
*** *** *** *** *** *** *** *** *** *** *** *** *** *** *** ***
Fatal signal 6 (SIGABRT), code -6 (SI_TKILL) fault addr 0x0
Thread: 12 \"main\"
Backtrace:
      #00 pc 0000000000044a10
";
        let c = analyze("", truncated, &[]).expect("partial dump parses");
        assert_eq!(c.signal, Some(6));
        assert_eq!(c.signal_name.as_deref(), Some("SIGABRT"));
        assert_eq!(c.frames.len(), 1);
        assert_eq!(c.unmapped_frames, 1);
    }
}
