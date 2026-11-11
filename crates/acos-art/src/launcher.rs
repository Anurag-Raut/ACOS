//! Running a DEX under real ART and turning the process outcome into data.
//!
//! Everything a task needs to decide "did Java code run correctly?" lives in
//! [`RunResult`]: exit status, captured stdout/stderr, wall time, the structured
//! liblog entries, and one [`Outcome`] + stable error `code`.
//!
//! Bounding is not optional (ADR-0018): every run gets a wall-clock timeout, a
//! process **group** kill on expiry, and rlimits applied in the child before
//! `exec` (`RLIMIT_AS`, `RLIMIT_CORE=0`, `RLIMIT_NOFILE`, `PDEATHSIG`).

use crate::capture::{stream_cap_from_env, TailBuffer};
use crate::logd::{LogEntry, LogStats, Logd};
use crate::watchdog::{MemoryWatchdog, HOST_MEMORY_LIMIT};
use crate::{ArtRoot, Error, Result};
use serde::{Deserialize, Serialize};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// What the guest process did, in ACOS terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Java `main` ran to completion (a deliberate nonzero exit, e.g. an
    /// uncaught `main` exception, still counts: it is the program's verdict).
    Ok,
    /// ART died: signal (SIGSEGV/SIGABRT/…) or a fatal `CHECK`/`LOG(FATAL)`.
    Crash,
    /// Wall-clock budget exhausted; the process group was killed.
    Timeout,
    /// ART could not do the work on this host (blocked/unimplemented syscall),
    /// as opposed to a bug in the guest.
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitMode {
    /// ART's default (JIT + interpreter, or AOT when an image is used).
    Default,
    /// `-Xint`: interpreter only, deterministic, slow.
    Int,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogMode {
    /// Only the shim's stderr fallback (no logd socket).
    Stderr,
    /// Bind `<data>/socket` at `/dev/socket` and capture liblog writes as
    /// structured entries.
    Logd,
}

/// One request to run a DEX.
#[derive(Debug, Clone)]
pub struct RunOpts {
    /// Host DEX files; each is copied into `<data>/task/` and referenced from
    /// the class path as `/data/task/<name>`.
    pub dex: Vec<PathBuf>,
    pub class: String,
    pub class_args: Vec<String>,
    pub jit: JitMode,
    pub timeout: Duration,
    /// `RLIMIT_AS` in MiB; 0 = unlimited (the default, and the only workable
    /// value for ART: Scudo reserves ~8.6 TB of *virtual* address space at
    /// start-up, so any `RLIMIT_AS` below that aborts with
    /// `Scudo ERROR: internal map failure (NO MEMORY)`).
    pub rlimit_as_mb: u64,
    pub log: LogMode,
    /// Explicit boot image directory (contains `x86_64/boot.art`).
    pub image: Option<PathBuf>,
    /// Auto-use `<data>/img` when it contains `x86_64/boot.art`.
    pub use_image: bool,
    /// Load `libartshim.so` (liblog sink + property service).
    pub shim: bool,
    /// Write the property table the shim serves.
    pub props: bool,
    pub extra_env: Vec<(String, String)>,
}

impl Default for RunOpts {
    fn default() -> Self {
        Self {
            dex: Vec::new(),
            class: "Main".to_string(),
            class_args: Vec::new(),
            jit: JitMode::Default,
            timeout: Duration::from_secs(60),
            rlimit_as_mb: 0,
            log: LogMode::Logd,
            image: None,
            use_image: true,
            shim: true,
            props: true,
            extra_env: Vec::new(),
        }
    }
}

/// The structured result of one ART run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    /// Process exit code (`None` when killed by a signal or on timeout).
    pub exit: Option<i32>,
    /// Terminating signal number, if any.
    pub signal: Option<i32>,
    pub outcome: Outcome,
    /// Stable code: `ART_FATAL_CHECK`, `ART_CRASH`, `ART_FATAL_LOG`,
    /// `UNSUPPORTED_SYSCALL`, `TIMEOUT`, `NONZERO_EXIT`, or `None`.
    pub code: Option<String>,
    pub wall_ms: u64,
    pub stdout: String,
    pub stderr: String,
    /// Structured liblog entries (only with [`LogMode::Logd`]).
    pub logs: Vec<LogEntry>,
    /// Human-readable notes: crash reason, suspicious log lines, which boot
    /// image was used, ignored `-Ximage`, …
    pub diagnostics: Vec<String>,
    pub class: String,
    pub dex: Vec<String>,
    pub image: Option<String>,
    pub argv: Vec<String>,
}

impl RunResult {
    pub fn succeeded(&self) -> bool {
        self.outcome == Outcome::Ok
    }
}

/// Run one DEX under ART.
pub fn run(root: &ArtRoot, opts: &RunOpts) -> Result<RunResult> {
    root.ensure_data()?;
    root.ensure_linkerconfig(false)?;
    // The device classpath pair (BOOTCLASSPATH + DEX2OATBOOTCLASSPATH) from the
    // image's own derive_classpath: ART CHECK-fails without it.
    root.ensure_classpath_env(false)?;

    // Property table: present or absent is the switch the shim sees.
    let props_path = root.data.join(crate::props::PROPS_FILE);
    if opts.props {
        crate::Props::android34().write(&props_path)?;
    } else if props_path.exists() {
        std::fs::remove_file(&props_path)?;
    }

    let shim_built = if opts.shim { Some(crate::build_shim(&root.data.join("shim"))?) } else { None };
    let logd = match (opts.log, shim_built.is_some()) {
        (LogMode::Logd, true) => Some(Logd::start(&root.data.join("socket")).map_err(Error::Io)?),
        _ => None,
    };

    // Provision the DEX files: copy into the data dir so the bind source is a
    // regular file we own, then reference by device path.
    let task = root.data.join("task");
    std::fs::create_dir_all(&task)?;
    let mut cp_dev = Vec::new();
    let mut dex_names = Vec::new();
    for (i, dex) in opts.dex.iter().enumerate() {
        let file = dex.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| {
            format!("classes{i}.dex")
        });
        let name = format!("{i:02}_{file}");
        std::fs::copy(dex, task.join(&name))?;
        cp_dev.push(format!("/data/task/{name}"));
        dex_names.push(dex.display().to_string());
    }

    let image = if opts.use_image {
        opts.image.clone().or_else(|| {
            let d = root.data.join("img");
            d.join("x86_64/boot.art").is_file().then_some(d)
        })
    } else {
        opts.image.clone()
    };
    let image_ready = image
        .as_ref()
        .map(|d| d.join("x86_64/boot.art").is_file())
        .unwrap_or(false);

    let mut sb = root.sandbox();
    sb.with_shim(shim_built.is_some());
    sb.dir("/data/task");
    sb.args(Vec::new());
    if image_ready {
        sb.with_image(image.clone());
    }
    for (k, v) in &opts.extra_env {
        sb.env(k, v);
    }
    for dev in &cp_dev {
        let host = task.join(dev.trim_start_matches("/data/task/"));
        sb.ro_bind(&host, dev);
    }

    let mut argv: Vec<String> = Vec::new();
    if opts.jit == JitMode::Int {
        argv.push("-Xint".to_string());
    }
    if image_ready {
        argv.push("-Ximage:/data/img/boot.art".to_string());
    }
    if !cp_dev.is_empty() {
        argv.push("-cp".to_string());
        argv.push(cp_dev.join(":"));
    }
    argv.push(opts.class.clone());
    argv.extend(opts.class_args.iter().cloned());

    // Must be the **/system/apex/** path: bionic's linker keys the per-APEX
    // namespace lookup off it (the `/apex/<module>` spelling makes it fall back
    // to the default namespace, which cannot see libnativehelper.so).
    let dalvik = "/system/apex/com.android.art/bin/dalvikvm64";
    let mut cmd = sb.command(dalvik, &argv)?;
    let rlimit_as = opts.rlimit_as_mb;
    unsafe {
        cmd.pre_exec(move || {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong, 0, 0, 0);
            if rlimit_as > 0 {
                let lim = libc::rlimit {
                    rlim_cur: rlimit_as * 1024 * 1024,
                    rlim_max: rlimit_as * 1024 * 1024,
                };
                if libc::setrlimit(libc::RLIMIT_AS, &lim) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            let core = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            libc::setrlimit(libc::RLIMIT_CORE, &core);
            let nofile = libc::rlimit { rlim_cur: 4096, rlim_max: 4096 };
            libc::setrlimit(libc::RLIMIT_NOFILE, &nofile);
            Ok(())
        });
    }
    cmd.process_group(0);
    let mut cmd = cmd; // pre_exec/process_group return &mut Command
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut command: Command = cmd;

    let started = Instant::now();
    let mut child = command.spawn().map_err(|e| Error::Tool(format!("spawning bwrap: {e}")))?;
    let pid = child.id() as i32;

    // Captured with a hard ceiling: a guest that loops on printf must not be
    // able to grow this process (R1: read_to_end was the unbounded allocation).
    let cap = stream_cap_from_env();
    let (tx, rx) = mpsc::channel::<(u8, TailBuffer)>();
    let out_pipe = child.stdout.take().expect("piped stdout");
    let tx_out = tx.clone();
    let out_thread = std::thread::spawn(move || {
        let buf = crate::capture::drain_to_tail(out_pipe, cap);
        let _ = tx_out.send((0u8, buf));
    });
    let err_pipe = child.stderr.take().expect("piped stderr");
    let tx_err = tx.clone();
    let err_thread = std::thread::spawn(move || {
        let buf = crate::capture::drain_to_tail(err_pipe, cap);
        let _ = tx_err.send((1u8, buf));
    });

    // The launcher's own ceiling: a leak the caps do not know about still ends
    // as a structured HOST_MEMORY_LIMIT instead of a kernel OOM kill.
    let dog = MemoryWatchdog::from_env();
    if let Some(d) = &dog {
        d.arm_process_group(pid);
    }

    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                return Err(Error::Io(e));
            }
        }
        if let Some(d) = &dog {
            if d.trip().is_some() {
                // The watchdog already SIGKILLed the group; reap it and report
                // the structured outcome.
                let _ = child.wait();
                break None;
            }
        }
        if started.elapsed() >= opts.timeout {
            timed_out = true;
            // Kill the whole group: bwrap + dalvikvm64 + ART threads.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let wall_ms = started.elapsed().as_millis() as u64;

    let mut stdout_buf = TailBuffer::with_cap(cap);
    let mut stderr_buf = TailBuffer::with_cap(cap);
    for _ in 0..2 {
        match rx.recv_timeout(Duration::from_secs(3)) {
            Ok((0, b)) => stdout_buf = b,
            Ok((_, b)) => stderr_buf = b,
            Err(_) => break,
        }
    }
    let _ = out_thread.join();
    let _ = err_thread.join();
    drop(tx);

    let batch = logd.map(|l| l.finish_capped()).unwrap_or_default();
    let log_stats: LogStats = batch.stats;
    let batch_log_first_dropped = batch.first_dropped.clone();
    let logs = batch.entries;
    let stdout = stdout_buf.to_text();
    let stderr = stderr_buf.to_text();

    // bwrap re-exits with 128+signal when the *guest* died from a signal, so
    // undo that remap: ART dying of SIGSEGV must read as signal 11, not "exit
    // 139".
    let raw_exit = status.and_then(|s| s.code());
    let exit = match raw_exit {
        Some(c) if (129..=192).contains(&c) => None,
        other => other,
    };
    let mut signal = status.and_then(|s| {
        #[cfg(unix)]
        {
            std::os::unix::process::ExitStatusExt::signal(&s)
        }
        #[cfg(not(unix))]
        {
            None
        }
    });

    if signal.is_none() {
        if let Some(c) = raw_exit {
            if (129..=192).contains(&c) {
                signal = Some(c - 128);
            }
        }
    }
    let (mut outcome, mut code, mut diagnostics) =
        classify(exit, signal, timed_out, &stderr, &logs, opts.timeout);
    // A watchdog kill is a host-side resource outcome, not a guest crash: it
    // outranks whatever the missing exit status looked like.
    if let Some(d) = &dog {
        if let Some(trip) = d.trip() {
            outcome = Outcome::Unsupported;
            code = Some(HOST_MEMORY_LIMIT.to_string());
            diagnostics.push(format!(
                "{HOST_MEMORY_LIMIT}: launcher RSS {} MiB exceeded the {} MiB ceiling \
                 (ACOS_ART_RSS_MAX); guest process group {:?} killed",
                trip.rss_bytes / 1024 / 1024,
                trip.limit_bytes / 1024 / 1024,
                trip.killed_pgid
            ));
        }
    }
    let _ = &mut code;
    if let Some(img) = &image {
        if image_ready {
            diagnostics.push(format!("boot image: {}", img.display()));
        } else {
            diagnostics.push(format!(
                "boot image ignored (no x86_64/boot.art in {}): running imageless",
                img.display()
            ));
        }
    }
    if let Some(s) = shim_built {
        diagnostics.push(format!("shim: {}", s.display()));
    }
    if opts.log == LogMode::Logd {
        diagnostics.push(log_stats.summary());
        if log_stats.truncated() {
            if let Some(e) = &batch_log_first_dropped {
                diagnostics.push(format!(
                    "logd: oldest entries dropped; first dropped was [{}] {}: {}",
                    e.priority_name(),
                    e.tag,
                    crate::capture::head(&e.message, 200)
                ));
            }
        }
    }
    let _ = &mut diagnostics;

    Ok(RunResult {
        exit,
        signal,
        outcome,
        code,
        wall_ms,
        stdout,
        stderr,
        logs,
        diagnostics,
        class: opts.class.clone(),
        dex: dex_names,
        image: image_ready.then(|| image.as_ref().unwrap().display().to_string()),
        argv: std::iter::once(dalvik.to_string()).chain(argv).collect(),
    })
}

/// Map the raw process/ART evidence onto the ACOS outcome vocabulary.
pub fn classify(
    exit: Option<i32>,
    signal: Option<i32>,
    timed_out: bool,
    stderr: &str,
    logs: &[LogEntry],
    timeout: Duration,
) -> (Outcome, Option<String>, Vec<String>) {
    let mut diags = Vec::new();
    if timed_out {
        diags.push(format!("killed after {} ms timeout", timeout.as_millis()));
        return (Outcome::Timeout, Some("TIMEOUT".into()), diags);
    }

    let log_text: String = logs
        .iter()
        .map(|e| format!("{}: {}\n", e.tag, e.message))
        .collect();
    let combined = format!("{stderr}\n{log_text}");
    let fatal_log = logs.iter().find(|e| e.is_fatal()).cloned();
    let check_text = stderr
        .lines()
        .find(|l| l.contains("Check failed") || l.contains("CHECK failed"))
        .map(str::to_string);

    if let Some(sig) = signal {
        let name = signal_name(sig);
        diags.push(format!("terminated by signal {sig} ({name})"));
        if sig == libc::SIGSYS {
            if let Some(msg) = stderr.lines().find(|l| !l.trim().is_empty()) {
                diags.push(format!("first stderr line: {msg}"));
            }
            return (Outcome::Unsupported, Some("UNSUPPORTED_SYSCALL".into()), diags);
        }
        if let Some(e) = &fatal_log {
            diags.push(format!("ART [{}] {}: {}", e.priority_name(), e.tag, e.message));
        }
        // A deliberate failure (exit 1 + uncaught exception in main) is the
        // program's verdict, not an engine failure — and ART logs fatal-looking
        // entries while unwinding, so this has to be checked first.
        if stderr.contains("ClassNotFoundException") || stderr.contains("NoClassDefFoundError") {
            diags.push("class not found on the given class path".into());
            return (Outcome::Crash, Some("ART_NO_CLASS".into()), diags);
        }
        if exit == Some(1) && stderr.contains("Exception in thread \"main\"") {
            diags.push("uncaught exception in main (expected for a failing program)".into());
            return (Outcome::Ok, None, diags);
        }
        if combined.contains("Scudo ERROR") || combined.contains("out of memory") {
            diags.push("host memory limit hit (see ACOS_HOST_RSS_MAX / guarded.sh; RLIMIT_AS is not usable for ART)".into());
            return (Outcome::Unsupported, Some("HOST_MEMORY_LIMIT".into()), diags);
        }
        // A fatal CHECK now aborts for real (the shim no longer suppresses the
        // aborter), so the signal and the fatal log entry arrive together.
        let fatal_check = logs.iter().find(|e| e.message.contains("Check failed") || e.message.contains("CHECK failed"));
        if let Some(e) = fatal_check.or(fatal_log.as_ref()) {
            if fatal_check.is_some() || e.tag == "art_aborter" {
                diags.push(format!("ART CHECK failed: {}", e.message));
                return (Outcome::Crash, Some("ART_FATAL_CHECK".into()), diags);
            }
        }
        if let Some(c) = &check_text {
            diags.push(c.clone());
            return (Outcome::Crash, Some("ART_FATAL_CHECK".into()), diags);
        }
        return (Outcome::Crash, Some("ART_CRASH".into()), diags);
    }

    // A deliberate failure (exit 1 + uncaught exception in main) is the
    // program's verdict, not an engine failure — and ART logs fatal-looking
    // entries while unwinding, so this has to be checked first.
    if stderr.contains("ClassNotFoundException") || stderr.contains("NoClassDefFoundError") {
        diags.push("class not found on the given class path".into());
        return (Outcome::Crash, Some("ART_NO_CLASS".into()), diags);
    }
    if exit == Some(1) && stderr.contains("Exception in thread \"main\"") {
        diags.push("uncaught exception in main (expected for a failing program)".into());
        return (Outcome::Ok, None, diags);
    }
    if combined.contains("Scudo ERROR") || combined.contains("out of memory") {
        diags.push("host memory limit hit (see ACOS_HOST_RSS_MAX / guarded.sh; RLIMIT_AS is not usable for ART)".into());
        return (Outcome::Unsupported, Some("HOST_MEMORY_LIMIT".into()), diags);
    }
    if let Some(c) = &check_text {
        diags.push(c.clone());
        return (Outcome::Crash, Some("ART_FATAL_CHECK".into()), diags);
    }
    if let Some(e) = &fatal_log {
        diags.push(format!("ART [{}] {}: {}", e.priority_name(), e.tag, e.message));
        // An ART fatal log without a signal is still a crash, unless the process
        // exited 0 (some LOG(FATAL) paths are recovered).
        if exit != Some(0) {
            let is_check = e.tag == "art_aborter"
                || e.message.contains("Check failed")
                || e.message.contains("CHECK failed");
            let code = if is_check { "ART_FATAL_CHECK" } else { "ART_FATAL_LOG" };
            return (Outcome::Crash, Some(code.into()), diags);
        }
    }

    // "Unsupported" is reserved for the host failing ART (syscall/namespace/
    // seccomp), never for a guest bug or an ordinary Java failure.
    for pat in [
        "Function not implemented",
        "Operation not permitted",
        "Permission denied",
        "unsupported",
        "UNSUPPORTED",
    ] {
        if stderr.contains(pat) {
            diags.push(format!("stderr mentions \"{pat}\""));
            break;
        }
    }

    match exit {
        Some(0) => (Outcome::Ok, None, diags),
        Some(c) => {
            diags.push(format!("exit code {c}"));
            (Outcome::Crash, Some("NONZERO_EXIT".into()), diags)
        }
        None => (Outcome::Crash, Some("ART_CRASH".into()), diags),
    }
}

fn signal_name(sig: i32) -> &'static str {
    match sig {
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGILL => "SIGILL",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGTERM => "SIGTERM",
        libc::SIGSYS => "SIGSYS",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGPIPE => "SIGPIPE",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logd::LogEntry;

    fn entry(prio: i32, tag: &str, msg: &str) -> LogEntry {
        LogEntry { prio, tag: tag.into(), message: msg.into() }
    }

    #[test]
    fn timeout_wins() {
        let (o, c, _) = classify(None, None, true, "", &[], Duration::from_secs(1));
        assert_eq!(o, Outcome::Timeout);
        assert_eq!(c.as_deref(), Some("TIMEOUT"));
    }

    #[test]
    fn segv_is_art_crash() {
        let (o, c, d) = classify(None, Some(libc::SIGSEGV), false, "", &[], Duration::from_secs(1));
        assert_eq!(o, Outcome::Crash);
        assert_eq!(c.as_deref(), Some("ART_CRASH"));
        assert!(d.iter().any(|x| x.contains("SIGSEGV")));
    }

    #[test]
    fn check_failure_is_art_fatal_check() {
        let (o, c, _) = classify(
            Some(1),
            None,
            false,
            "Check failed: !location.empty() No mainline framework library found",
            &[],
            Duration::from_secs(1),
        );
        assert_eq!(o, Outcome::Crash);
        assert_eq!(c.as_deref(), Some("ART_FATAL_CHECK"));
    }

    #[test]
    fn sigabrt_from_fatal_check_is_art_fatal_check() {
        // Fatal-abort behaviour restored: the CHECK now really dies of SIGABRT,
        // and the recorded message must still classify as a CHECK, not CRASH.
        let logs = [entry(7, "", "Check failed: !location.empty() No mainline framework library found")];
        let (o, c, d) = classify(None, Some(libc::SIGABRT), false, "", &logs, Duration::from_secs(1));
        assert_eq!(o, Outcome::Crash);
        assert_eq!(c.as_deref(), Some("ART_FATAL_CHECK"));
        assert!(d.iter().any(|x| x.contains("SIGABRT")));
    }

    #[test]
    fn aborter_log_entry_is_fatal() {
        let logs = [entry(6, "art_aborter", "Check failed: x")];
        let (o, c, _) = classify(Some(1), None, false, "", &logs, Duration::from_secs(1));
        assert_eq!(o, Outcome::Crash);
        assert_eq!(c.as_deref(), Some("ART_FATAL_CHECK"));
    }

    #[test]
    fn uncaught_main_exception_is_ok() {
        let (o, c, _) = classify(
            Some(1),
            None,
            false,
            "Exception in thread \"main\" Main$Fatal: fatal without args",
            &[],
            Duration::from_secs(1),
        );
        assert_eq!(o, Outcome::Ok);
        assert!(c.is_none());
    }

    #[test]
    fn sigsys_is_unsupported() {
        let (o, c, _) = classify(None, Some(libc::SIGSYS), false, "", &[], Duration::from_secs(1));
        assert_eq!(o, Outcome::Unsupported);
        assert_eq!(c.as_deref(), Some("UNSUPPORTED_SYSCALL"));
    }
}
