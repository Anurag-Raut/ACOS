//! Cleanup and limit proof for the real launcher (`acos-art` as a black box).
//!
//! Three separate incidents motivate this suite:
//!
//! * the 2026-11-11 OOM (`results/art-runtime/R1.md`) — the launcher's *own*
//!   RSS grew unbounded off a guest log loop until the kernel killed the desktop
//!   session, so the fix must be visible from outside the process;
//! * `results/art-runtime/S3.md` — a launcher that dies must not leave the guest
//!   behind, which is what bwrap's `--die-with-parent` is for, but "the flag is
//!   in the argv" is not the same claim as "the guest is gone in 3 s";
//! * the everyday one nobody writes down — the run ends, and the host keeps the
//!   processes, the listening socket, the memfds or the scratch directory.
//!
//! Every assertion here is made from *outside* the launcher (a full `/proc`
//! scan, `/proc/<pid>/status:VmHWM`, a datagram flood into the real logd sink),
//! so the tests cannot be satisfied by an internal counter that drifted from
//! behaviour. `tools/art-runtime/leakcheck.sh` is the shell twin of the process
//! scan and is the tool to run around the whole suite.
//!
//! The suite skips (with a reason, exit 0) when the ART sysroot or the cached
//! guest dex is absent: those are 1 GB+ host artefacts produced by
//! `tools/art-sysroot.sh` / one warm `acos-art render-app`, not something a
//! `cargo test` may download or build.
//!
//! ART is started through `tools/guarded.sh` by the acceptance command; inside
//! the test, runs are serialised on one mutex so a suite run never has two
//! guests alive at once (the owner's host-safety rule).

use std::collections::BTreeSet;
use std::io::Write;
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Serialises ART-spawning tests: only one guest at a time on this host.
static ART_LOCK: Mutex<()> = Mutex::new(());

fn art_guard() -> MutexGuard<'static, ()> {
    ART_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Process names an ART run may leave behind, matched on `comm` **or** cmdline.
const PATTERNS: &[&str] = &["dalvikvm64", "bwrap", "acos-art", "SessionMain"];

/// The launcher ceiling this suite proves for the log-spam scenario.
const LAUNCHER_PEAK_LIMIT_MB: f64 = 256.0;

// ---------------------------------------------------------------------------
// Repository / fixture layout
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate lives at <repo>/crates/acos-art")
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_acos-art")
}

fn sysroot() -> PathBuf {
    std::env::var_os("ACOS_ART_SYSROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join(".cache/art/sysroot"))
}

fn warm_data() -> PathBuf {
    std::env::var_os("ACOS_ART_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join(".cache/art/data"))
}

/// `None` = the host lacks the (large, out-of-repo-build) ART inputs; the test
/// prints why and passes without asserting anything.
fn require_art() -> Option<()> {
    let sysroot = sysroot();
    if !sysroot.join("system/apex").is_dir() {
        eprintln!(
            "SKIP cleanup test: no ART sysroot at {} — run tools/art-sysroot.sh",
            sysroot.display()
        );
        return None;
    }
    if !warm_data().join("app/guest/classes.dex").is_file() {
        eprintln!(
            "SKIP cleanup test: no cached guest dex under {} — run \
             `acos-art render-app --apk fixtures/build/apps/counter.apk` once",
            warm_data().display()
        );
        return None;
    }
    Some(())
}

/// A scratch dir under the repo `.cache/` (never `/tmp`), fresh per test.
fn scratch(tag: &str) -> PathBuf {
    let dir = repo_root()
        .join(".cache/h1-tests")
        .join(format!("{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir under .cache/");
    dir
}

/// A private copy of the writable `/data` tree, so a test can never race another
/// agent's run and its leftovers are removed with the scratch dir.
fn seed_data(scratch: &Path) -> PathBuf {
    let data = scratch.join("data");
    let warm = warm_data();
    let st = Command::new("cp")
        .arg("-a")
        .arg(&warm)
        .arg(&data)
        .status()
        .expect("cp is available");
    assert!(st.success(), "could not seed /data from {}", warm.display());
    data
}

fn apk() -> PathBuf {
    repo_root().join("fixtures/build/apps/counter.apk")
}

// ---------------------------------------------------------------------------
// /proc scanning: processes, RSS, survivors
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Proc {
    pid: u32,
    /// `/proc/<pid>/stat:starttime` — makes identity immune to pid reuse.
    start: u64,
    ppid: u32,
    state: char,
    comm: String,
    cmd: String,
}

impl Proc {
    fn key(&self) -> (u32, u64) {
        (self.pid, self.start)
    }
    /// A zombie holds no memory, no socket and no memfd; init reaps it. It is
    /// reported, but it does not make a run "leaking a process".
    fn is_zombie(&self) -> bool {
        self.state == 'Z'
    }
    fn is_guest(&self) -> bool {
        self.comm.contains("dalvikvm64") || self.cmd.contains("dalvikvm64")
    }
}

fn read_cmdline(dir: &Path) -> String {
    std::fs::read(dir.join("cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn scan_procs() -> Vec<Proc> {
    let root = repo_root();
    let root_s = root.to_string_lossy().to_string();
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return out };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let dir = entry.path();
        let (Ok(comm), Ok(stat)) = (
            std::fs::read_to_string(dir.join("comm")),
            std::fs::read_to_string(dir.join("stat")),
        ) else {
            continue;
        };
        let comm = comm.trim().to_string();
        // Fields after "(comm)": 0 state, 1 ppid, … 19 starttime.
        let Some(rest) = stat.rfind(')').and_then(|i| stat.get(i + 2..)) else { continue };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 20 {
            continue;
        }
        let Ok(start) = fields[19].parse::<u64>() else { continue };
        let cmd = read_cmdline(&dir);
        if !PATTERNS.iter().any(|p| comm.contains(p) || cmd.contains(p)) {
            continue;
        }
        // Every desktop has flatpak/snap bwrap wrappers coming and going; an ART
        // bwrap always names a path inside this repo (sysroot, data, shim).
        if comm == "bwrap" && !cmd.contains(&root_s) {
            continue;
        }
        out.push(Proc {
            pid,
            start,
            ppid: fields[1].parse().unwrap_or(0),
            state: fields[0].chars().next().unwrap_or('?'),
            comm,
            cmd,
        });
    }
    out
}

fn snapshot() -> BTreeSet<(u32, u64)> {
    scan_procs().into_iter().map(|p| p.key()).collect()
}

fn new_procs(before: &BTreeSet<(u32, u64)>) -> Vec<Proc> {
    let mut v: Vec<Proc> = scan_procs()
        .into_iter()
        .filter(|p| !before.contains(&p.key()) && !p.is_zombie())
        .collect();
    v.sort_by_key(|p| p.pid);
    v
}

fn describe(procs: &[Proc]) -> String {
    procs
        .iter()
        .map(|p| {
            format!(
                "\n  pid={} ppid={} state={} comm={} cmd={}",
                p.pid, p.ppid, p.state, p.comm, p.cmd
            )
        })
        .collect()
}

/// Poll until no process that appeared after `before` is still alive, or
/// `within` elapses. Returns the survivors (empty = clean).
fn wait_until_clean(before: &BTreeSet<(u32, u64)>, within: Duration) -> Vec<Proc> {
    let deadline = Instant::now() + within;
    loop {
        let survivors = new_procs(before);
        if survivors.is_empty() || Instant::now() >= deadline {
            return survivors;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Best-effort cleanup so a failing assertion cannot itself leak a guest.
fn reap(procs: &[Proc]) {
    for p in procs {
        unsafe {
            libc::kill(p.pid as i32, libc::SIGKILL);
        }
    }
}

/// `/proc/<pid>/status:<field>` in KiB (e.g. `VmHWM` = peak RSS).
fn status_kb(pid: u32, field: &str) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|l| {
        l.strip_prefix(field)
            .and_then(|rest| rest.trim_start_matches(':').split_whitespace().next())
            .and_then(|kb| kb.parse().ok())
    })
}

/// Kills the child on drop, so a panicking test never leaves a guest behind.
struct Reaper(Child);

impl Reaper {
    fn pid(&self) -> u32 {
        self.0.id()
    }
    fn try_wait(&mut self) -> Option<std::process::ExitStatus> {
        self.0.try_wait().ok().flatten()
    }
    /// Wait for exit up to `within`, then SIGKILL. Returns whether it exited by
    /// itself.
    fn wait_or_kill(&mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if self.try_wait().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
        false
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn dir_entries(dir: &Path) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            set.insert(e.file_name().to_string_lossy().to_string());
        }
    }
    set
}

/// `acos-art <sub> --data <dir> …`: `acos-art` parses its subcommand first, so
/// common options (`--data`) must follow it.
fn launcher(sub: &str, data: &Path) -> Command {
    let mut c = Command::new(bin());
    c.env("ACOS_ART_QUIET_GUARD", "1");
    c.arg(sub).arg("--data").arg(data);
    c.stdin(Stdio::null());
    c
}

// ---------------------------------------------------------------------------
// 1. The normal path ends where it started
// ---------------------------------------------------------------------------

#[test]
fn render_app_leaves_no_processes_or_scratch_behind() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let scratch = scratch("render");
    let data = seed_data(&scratch);
    let out = scratch.join("out");

    // render-app must not add a single per-session scratch directory: it is the
    // path with no session at all, so anything under `<data>/session` is a leak.
    let sessions_before = dir_entries(&data.join("session"));
    let before = snapshot();

    let st = launcher("render-app", &data)
        .arg("--apk")
        .arg(apk())
        .arg("--out")
        .arg(&out)
        .arg("--timeout")
        .arg("120")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn acos-art render-app");
    assert!(st.success(), "render-app failed: {st:?}");
    let frame = out.join("frame.png");
    let meta = std::fs::metadata(&frame)
        .unwrap_or_else(|e| panic!("no first frame at {}: {e}", frame.display()));
    assert!(meta.len() > 1024, "frame.png is only {} bytes", meta.len());

    let leaked = wait_until_clean(&before, Duration::from_secs(3));
    let detail = describe(&leaked);
    reap(&leaked);
    assert!(
        leaked.is_empty(),
        "render-app left {} process(es) behind after a successful run:{detail}",
        leaked.len()
    );

    let sessions_after = dir_entries(&data.join("session"));
    let new: Vec<&String> = sessions_after.difference(&sessions_before).collect();
    assert!(new.is_empty(), "render-app left scratch behind under {}/session: {new:?}", data.display());

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 2. SIGKILL the launcher: the guest must die with it (S3 --die-with-parent)
// ---------------------------------------------------------------------------

#[test]
fn sigkill_of_the_launcher_takes_the_guest_and_bwrap_with_it() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let scratch = scratch("sigkill");
    let data = seed_data(&scratch);
    let sock = scratch.join("session.sock");
    let log = std::fs::File::create(scratch.join("session.log")).expect("session log");

    let before = snapshot();

    let mut cmd = launcher("session", &data);
    cmd.arg("--apk")
        .arg(apk())
        .arg("--out")
        .arg(scratch.join("out"))
        .arg("--socket")
        .arg(&sock)
        .arg("--session-id")
        .arg("h1sigkill")
        .arg("--timeout")
        .arg("120")
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    // Own session, so a stray terminal signal or the test harness's group kill
    // cannot mask (or cause) what this test measures.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = Reaper(cmd.spawn().expect("spawn acos-art session"));
    let launcher_pid = child.pid();

    // Wait for the real guest to be up: a *new* dalvikvm64 after our snapshot.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut guests = Vec::new();
    while Instant::now() < deadline {
        guests = new_procs(&before).into_iter().filter(Proc::is_guest).collect();
        if !guests.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !guests.is_empty(),
        "no dalvikvm64 guest appeared within 60 s; session log at {}",
        scratch.join("session.log").display()
    );
    // Everything the run created, guest and bwrap alike.
    let victims = new_procs(&before);
    let victim_detail = describe(&victims);

    unsafe {
        libc::kill(launcher_pid as i32, libc::SIGKILL);
    }
    let _ = child.0.wait();

    let survivors = wait_until_clean(&before, Duration::from_secs(3));
    let detail = describe(&survivors);
    reap(&survivors);
    assert!(
        survivors.is_empty(),
        "SIGKILL of the launcher (pid {launcher_pid}) left {} process(es) alive after 3 s \
         (S3: --die-with-parent must take the guest with the launcher).\n\
         Started by the run:{victim_detail}\nSurvivors:{detail}",
        survivors.len()
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 3. The R1 watchdog: a structured HOST_MEMORY_LIMIT, and no orphans
// ---------------------------------------------------------------------------

#[test]
fn r1_watchdog_reports_host_memory_limit_and_leaves_no_orphans() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let dex = repo_root().join("fixtures/build/dexprogs/01_hello/classes.dex");
    if !dex.is_file() {
        eprintln!("SKIP watchdog test: no {} (build the dex corpus)", dex.display());
        return;
    }
    let scratch = scratch("watchdog");
    let data = seed_data(&scratch);
    let before = snapshot();

    // A 1 MiB launcher ceiling: crossed before the guest even exists. The
    // structured RLIMIT_DATA backstop must be off, otherwise the launcher would
    // fail to allocate before the watchdog can report (R1 layer 3 vs layer 2).
    let out = launcher("run", &data)
        .env("ACOS_ART_RSS_MAX", "1M")
        .env("ACOS_ART_RLIMIT_DATA", "0")
        .arg("--dex")
        .arg(&dex)
        .arg("--class")
        .arg("Main")
        .arg("--json")
        .arg("--timeout")
        .arg("120")
        .output()
        .expect("spawn acos-art run");

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(!out.status.success(), "a 1 MiB ceiling must not end in success (rc={:?})", out.status.code());
    // The structured result is the contract: a kill that is not *reported* is
    // the incident again (a silent OOM victim).
    let json_start = stdout.find('{').unwrap_or_else(|| panic!("no JSON on stdout: {stdout}"));
    let json: serde_json::Value =
        serde_json::from_str(&stdout[json_start..]).expect("run --json emits valid JSON");
    assert_eq!(json["outcome"], "unsupported", "json={json}");
    assert_eq!(json["code"], "HOST_MEMORY_LIMIT", "json={json}");
    let diags = json["diagnostics"].to_string();
    assert!(diags.contains("HOST_MEMORY_LIMIT"), "diagnostics={diags}");
    assert!(diags.contains("ACOS_ART_RSS_MAX"), "the trip must name its knob: {diags}");
    assert!(
        stderr.contains("HOST_MEMORY_LIMIT"),
        "the watchdog must say so on stderr too: {stderr}"
    );

    let leaked = wait_until_clean(&before, Duration::from_secs(3));
    let detail = describe(&leaked);
    reap(&leaked);
    assert!(
        leaked.is_empty(),
        "the watchdog kill left {} process(es) alive after 3 s (the report is not a cleanup):{detail}",
        leaked.len()
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 4. The log-spam scenario: the launcher's peak RSS stays bounded
// ---------------------------------------------------------------------------

/// One frame in the shim's logd format: `[prio][tlen:u16][mlen:u16]pad tag msg`.
fn log_frame(msg_len: usize) -> Vec<u8> {
    let mut f = vec![6u8, 3, 0];
    f.extend_from_slice(&(msg_len as u16).to_le_bytes());
    f.push(0);
    f.extend_from_slice(b"art");
    f.extend(std::iter::repeat(b'x').take(msg_len));
    f
}

#[test]
fn log_spam_keeps_the_launcher_peak_rss_under_256_mib() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let scratch = scratch("logspam");
    let data = seed_data(&scratch);
    let sock = scratch.join("session.sock");
    let log = std::fs::File::create(scratch.join("session.log")).expect("session log");
    let before = snapshot();

    let mut cmd = launcher("session", &data);
    cmd.arg("--apk")
        .arg(apk())
        .arg("--out")
        .arg(scratch.join("out"))
        .arg("--socket")
        .arg(&sock)
        .arg("--session-id")
        .arg("h1spam")
        .arg("--timeout")
        .arg("180")
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    let mut child = Reaper(cmd.spawn().expect("spawn acos-art session"));
    let pid = child.pid();

    // The launcher's logd binds a per-session `<data>/session/<id>/socket/logdw`;
    // once it exists the guest is up and the sink is the real one R1 hit.
    let logdw = data.join("session/h1spam/socket/logdw");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !logdw.exists() {
        if Instant::now() >= deadline {
            let tail = std::fs::read_to_string(scratch.join("session.log")).unwrap_or_default();
            panic!("no logd sink at {} within 60 s:\n{tail}", logdw.display());
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    // Peak RSS from outside (`VmHWM` = the kernel's own high-water mark), sampled
    // until the flood is over.
    let peak_kb = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = {
        let peak_kb = Arc::clone(&peak_kb);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Some(kb) = status_kb(pid, "VmHWM") {
                    peak_kb.fetch_max(kb, Ordering::Relaxed);
                } else {
                    break; // the launcher exited
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };

    // The R1 flood: the guest's liblog loop with nothing draining it. 900-byte
    // entries into the real sink, at least 300 MiB, so an unbounded host (the
    // regression) would blow past the 256 MiB ceiling rather than nudge it.
    let frame = log_frame(900);
    let target: u64 = 300 * 1024 * 1024;
    let sock_dg = UnixDatagram::unbound().expect("unbound datagram socket");
    sock_dg.set_nonblocking(true).expect("non-blocking logdw writer");
    sock_dg.connect(&logdw).expect("connect logdw");
    let mut sent: u64 = 0;
    let mut spins: u64 = 0;
    let budget = Instant::now() + Duration::from_secs(40);
    while sent < target && Instant::now() < budget {
        match sock_dg.send(&frame) {
            Ok(n) => {
                sent += n as u64;
                spins = 0;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // The sink is behind: this is the incident's shape (producer
                // faster than the reader), just bounded by the cap.
                spins += 1;
                if spins % 32 == 0 {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            Err(e) => panic!("logdw send failed after {sent} bytes: {e}"),
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    stop.store(true, Ordering::Relaxed);
    let _ = sampler.join();

    let peak_mb = peak_kb.load(Ordering::Relaxed) as f64 / 1024.0;
    let sent_mb = sent as f64 / 1024.0 / 1024.0;

    // Close the session politely, then insist it exits.
    if let Ok(mut s) = UnixStream::connect(&sock) {
        let _ = s.write_all(b"{\"cmd\":\"close\"}\n");
        let _ = s.flush();
    }
    let exited = child.wait_or_kill(Duration::from_secs(10));

    let survivors = wait_until_clean(&before, Duration::from_secs(3));
    let detail = describe(&survivors);
    reap(&survivors);

    assert!(
        sent_mb > 200.0,
        "the flood did not happen: only {sent_mb:.0} MiB of log frames were accepted"
    );
    // The bound is the point: R1 grew ~400 MB/s in one thread. The cap must keep
    // the launcher flat no matter how much the guest writes.
    assert!(
        peak_mb < LAUNCHER_PEAK_LIMIT_MB,
        "launcher peak RSS was {peak_mb:.1} MiB after {sent_mb:.0} MiB of log spam \
         (limit {LAUNCHER_PEAK_LIMIT_MB:.0} MiB) — the R1 bound regressed"
    );
    assert!(exited, "the session did not exit after `close` within 10 s");
    assert!(
        survivors.is_empty(),
        "log-spam run left {} process(es) behind after close:{detail}",
        survivors.len()
    );
    eprintln!(
        "logspam: {sent_mb:.0} MiB of frames, launcher peak RSS {peak_mb:.1} MiB \
         (VmHWM, limit {LAUNCHER_PEAK_LIMIT_MB:.0} MiB)"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
