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
        // Match the process NAME, not "any command line that mentions it": on a
        // shared host `cargo build -p acos-art` (and every `bash -c … acos-art`
        // from another agent) matched the old cmdline test and was reported as a
        // leaked guest (W53; `guest_crash_still_removes_the_run_root` failed with
        // another agent's build in the list). A leaked launcher/guest/bwrap always
        // carries the binary name in `comm`, so the teeth are unchanged. The guest
        // runs inside bwrap with guest-side paths, so no repo-path test is possible
        // for it.
        if !PATTERNS.iter().any(|p| comm.contains(p)) {
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

    // S4: the per-run writable root (dalvik-cache, tmp, out, socket) must be
    // removed on the normal exit path, not merely emptied.
    let runs = dir_entries(&data.join("runs"));
    assert!(
        runs.is_empty(),
        "render-app left per-run data roots behind under {}/runs: {runs:?}",
        data.display()
    );

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

    // A SIGKILLed launcher cannot remove its own per-run root: that is exactly
    // the case the startup janitor exists for (S4). Prove the leftover is there
    // (not silently absent) and that one janitor sweep with a zero TTL removes
    // it while a live owner is untouched.
    let leftovers = dir_entries(&data.join("runs"));
    assert!(
        !leftovers.is_empty(),
        "expected the SIGKILLed run's data root to survive (it is the janitor's job)"
    );
    let jan = launcher("janitor", &data)
        .env("ACOS_ART_RUN_TTL_SECS", "0")
        .output()
        .expect("spawn acos-art janitor");
    assert!(jan.status.success(), "janitor failed: {jan:?}");
    let after = dir_entries(&data.join("runs"));
    assert!(
        after.is_empty(),
        "janitor left {} run dir(s) after a zero-TTL sweep: {after:?}",
        after.len()
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 2b. S4 canary: the guest cannot rewrite the shared read-only inputs
// ---------------------------------------------------------------------------

/// md5 of a file via `md5sum` (the crate's own helper is not exported to tests).
fn md5(path: &Path) -> String {
    let out = Command::new("md5sum")
        .arg(path)
        .output()
        .unwrap_or_else(|e| panic!("md5sum {}: {e}", path.display()));
    assert!(out.status.success(), "md5sum failed for {}", path.display());
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("md5sum output")
        .to_string()
}

fn toolchain_d8() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("D8") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Some(p) = std::env::var_os(var) {
            roots.push(PathBuf::from(p));
        }
    }
    roots.push(home.join("Android/Sdk"));
    for root in roots {
        let bt = root.join("build-tools");
        let Ok(rd) = std::fs::read_dir(&bt) else { continue };
        let mut versions: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        versions.sort();
        for v in versions.into_iter().rev() {
            let d8 = v.join("d8");
            if d8.is_file() {
                return Some(d8);
            }
        }
    }
    None
}

/// Compile the canary DEX (javac + d8). `None` = the host lacks the toolchain,
/// which is a skip, not a failure (`d8` comes from the Android SDK).
fn build_canary_dex(scratch: &Path) -> Option<PathBuf> {
    let Some(d8) = toolchain_d8() else {
        eprintln!("SKIP canary test: no d8 (Android build-tools) on this host");
        return None;
    };
    let src = scratch.join("canary/Canary.java");
    std::fs::create_dir_all(src.parent().unwrap()).ok()?;
    let targets = [
        "/data/img/x86_64/boot.art",
        "/data/system/environ/classpath",
        "/data/acos.properties",
        "/data/shim/libartshim.so",
        "/data/app/guest/classes.dex",
        "/linkerconfig/ld.config.txt",
    ];
    let mut java = String::from("import java.io.FileOutputStream;\n\npublic class Canary {\n  public static void main(String[] a) {\n    String[] t = {\n");
    for t in targets {
        java.push_str(&format!("      \"{t}\",\n"));
    }
    java.push_str(
        // Read first (the input must be *visible* to the guest), then try to write
        // (it must be denied). Without the read half, a FileNotFoundException
        // could just mean the path is absent from the sandbox.
        "    };\n    for (String p : t) {\n      try (java.io.FileInputStream in = new java.io.FileInputStream(p)) {\n        in.read();\n        System.out.println(\"READ \" + p);\n      } catch (Exception e) {\n        System.out.println(\"UNREADABLE \" + p + \" \" + e.getClass().getSimpleName());\n      }\n      try (FileOutputStream f = new FileOutputStream(p, true)) {\n        f.write(0);\n        System.out.println(\"WROTE \" + p);\n      } catch (Exception e) {\n        System.out.println(\"DENIED \" + p + \" \" + e.getClass().getSimpleName());\n      }\n    }\n  }\n}\n",
    );
    std::fs::write(&src, java).ok()?;
    let classes = scratch.join("canary/classes");
    let st = Command::new("javac")
        .arg("-d")
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?;
    if !st.success() {
        eprintln!("SKIP canary test: javac failed");
        return None;
    }
    let out = scratch.join("canary/dex");
    std::fs::create_dir_all(&out).ok()?;
    // d8 takes program *files* (class files or a jar), not a directory.
    let mut class_files: Vec<PathBuf> = std::fs::read_dir(&classes)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("class"))
        .collect();
    class_files.sort();
    if class_files.is_empty() {
        eprintln!("SKIP canary test: javac produced no class files");
        return None;
    }
    let st = Command::new(&d8)
        .arg("--output")
        .arg(&out)
        .args(&class_files)
        .status()
        .ok()?;
    if !st.success() {
        eprintln!("SKIP canary test: d8 failed ({})", d8.display());
        return None;
    }
    let dex = out.join("classes.dex");
    dex.is_file().then_some(dex)
}

#[test]
fn guest_cannot_overwrite_boot_art_or_the_classpath() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let scratch = scratch("canary");
    let data = seed_data(&scratch);
    let Some(dex) = build_canary_dex(&scratch) else {
        let _ = std::fs::remove_dir_all(&scratch);
        return;
    };

    // A real boot image if the host has one (`.cache/art-exp` from the
    // feasibility spike; `acos-art build-image` produces `.cache/art/data/img`).
    // The guest then really can attempt `boot.art`, not just a path that is
    // absent from the sandbox.
    let image = [".cache/art/data/img", ".cache/art-exp/data/img"]
        .iter()
        .map(|p| repo_root().join(p))
        .find(|p| p.join("x86_64/boot.art").is_file());

    let mut inputs: Vec<(&str, PathBuf)> = vec![
        ("/data/system/environ/classpath", data.join("system/environ/classpath")),
        ("/data/acos.properties", data.join("acos.properties")),
        ("/data/shim/libartshim.so", data.join("shim/libartshim.so")),
        ("/data/app/guest/classes.dex", data.join("app/guest/classes.dex")),
        ("/linkerconfig/ld.config.txt", data.join("linkerconfig/ld.config.txt")),
    ];
    if let Some(img) = &image {
        inputs.push(("/data/img/x86_64/boot.art", img.join("x86_64/boot.art")));
    }
    for (_, host) in &inputs {
        assert!(host.is_file(), "canary target missing on the host: {}", host.display());
    }
    let before: Vec<String> = inputs.iter().map(|(_, h)| md5(h)).collect();

    let mut cmd = launcher("run", &data);
    cmd.arg("--dex").arg(&dex).arg("--class").arg("Canary");
    if let Some(img) = &image {
        cmd.arg("--image").arg(img);
    }
    let out = cmd.output().expect("spawn acos-art run (canary)");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "the canary run itself failed (rc={:?})\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status.code()
    );

    for (guest, _host) in &inputs {
        assert!(
            stdout.contains(&format!("READ {guest}")),
            "the shared input {guest} is not even visible/readable in the guest\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            stdout.contains(&format!("DENIED {guest}")),
            "the guest was not denied write access to {guest}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            !stdout.contains(&format!("WROTE {guest}")),
            "the guest WROTE the shared input {guest} (S4 regression)\nstdout:\n{stdout}"
        );
    }
    let after: Vec<String> = inputs.iter().map(|(_, h)| md5(h)).collect();
    // The host-side proof: the bytes are identical, not just the guest's opinion.
    assert_eq!(before, after, "a shared input changed on the host during the canary run");

    // And the run root is gone, so the canary left nothing behind either.
    let runs = dir_entries(&data.join("runs"));
    assert!(runs.is_empty(), "canary run left per-run roots behind: {runs:?}");

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 2b-bis. A *guest crash* is still a normal host exit: the run root goes away
// ---------------------------------------------------------------------------

#[test]
fn guest_crash_still_removes_the_run_root() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let scratch = scratch("crash");
    let data = seed_data(&scratch);
    let sock = scratch.join("session.sock");
    let log = std::fs::File::create(scratch.join("session.log")).expect("session log");
    let before = snapshot();

    // A real session, then SIGKILL the *guest* (not the launcher): the launcher
    // must survive the crash, report it, and still remove its run root.
    let mut cmd = launcher("session", &data);
    cmd.arg("--apk")
        .arg(apk())
        .arg("--out")
        .arg(scratch.join("out"))
        .arg("--socket")
        .arg(&sock)
        .arg("--session-id")
        .arg("h1crash")
        // A session whose guest dies does not end early (it waits for the
        // protocol relay), so the wall-clock budget is what ends it. Short, so
        // this test is about the *cleanup*, not about waiting.
        .arg("--timeout")
        .arg("8")
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    let mut child = Reaper(cmd.spawn().expect("spawn acos-art session"));

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
        "no dalvikvm64 guest appeared within 60 s; log at {}",
        scratch.join("session.log").display()
    );
    for g in &guests {
        unsafe {
            libc::kill(g.pid as i32, libc::SIGKILL);
        }
    }

    // A session waits for its client, so close it (the guest is already gone;
    // the close must not resurrect it) and let the launcher finish on its own.
    if let Ok(mut s) = UnixStream::connect(&sock) {
        let _ = s.write_all(b"{\"cmd\":\"close\"}\n");
        let _ = s.flush();
    }
    assert!(
        child.wait_or_kill(Duration::from_secs(25)),
        "the launcher did not exit within 25 s of its guest being SIGKILLed (timeout 8 s)"
    );
    let survivors = wait_until_clean(&before, Duration::from_secs(3));
    let detail = describe(&survivors);
    reap(&survivors);
    assert!(survivors.is_empty(), "a crashed guest left processes behind:{detail}");

    let runs = dir_entries(&data.join("runs"));
    assert!(
        runs.is_empty(),
        "a crashed guest left its per-run data root behind: {runs:?}"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

// ---------------------------------------------------------------------------
// 2c. The startup janitor (the SIGKILL fallback) keeps live runs, reaps dead
// ---------------------------------------------------------------------------

fn own_pid_start() -> (u32, String) {
    let stat = std::fs::read_to_string("/proc/self/stat").expect("/proc/self/stat");
    let i = stat.rfind(')').expect("comm");
    let start = stat[i + 2..]
        .split_whitespace()
        .nth(19)
        .expect("starttime")
        .to_string();
    (std::process::id(), start)
}

fn age(path: &Path, spec: &str) {
    let st = Command::new("touch")
        .arg("-d")
        .arg(spec)
        .arg(path)
        .status()
        .expect("touch");
    assert!(st.success(), "touch -d {spec} {}", path.display());
}

#[test]
fn janitor_reaps_stale_runs_and_keeps_live_ones() {
    let _serial = art_guard();
    let Some(()) = require_art() else { return };
    let scratch = scratch("janitor");
    let data = seed_data(&scratch);
    let runs = data.join("runs");
    std::fs::create_dir_all(&runs).expect("create runs dir");

    let mk = |name: &str, owner: Option<String>| -> PathBuf {
        let d = runs.join(name);
        std::fs::create_dir_all(&d).expect("run dir");
        if let Some(o) = owner {
            std::fs::write(d.join("owner"), o).expect("owner file");
        }
        d
    };
    let stale = mk("stale", Some("pid=999999\nstart=1\ntag=stale\n".into()));
    let no_owner = mk("no-owner", None);
    let dead_fresh = mk("dead-fresh", Some("pid=999998\nstart=1\ntag=fresh\n".into()));
    let (pid, start) = own_pid_start();
    let live = mk("live", Some(format!("pid={pid}\nstart={start}\ntag=live\n")));
    for d in [stale.as_path(), no_owner.as_path(), live.as_path()] {
        age(&d.join("owner"), "2 hours ago");
        age(d, "2 hours ago");
    }

    // Default TTL (30 min): old + dead is reaped, old + live is not, fresh + dead
    // is kept in case its owner is still finishing.
    let out = launcher("janitor", &data).output().expect("janitor");
    assert!(out.status.success(), "janitor failed: {out:?}");
    assert!(!stale.exists(), "janitor kept a stale run dir with no live owner");
    assert!(!no_owner.exists(), "janitor kept an old run dir with no owner record");
    assert!(live.exists(), "janitor reaped a LIVE run dir (pid {pid})");
    assert!(dead_fresh.exists(), "janitor reaped a fresh run dir before the TTL");

    // Zero TTL: the SIGKILL case — the pid is gone, the age does not matter.
    let out = launcher("janitor", &data)
        .env("ACOS_ART_RUN_TTL_SECS", "0")
        .output()
        .expect("janitor (ttl=0)");
    assert!(out.status.success(), "janitor failed: {out:?}");
    assert!(!dead_fresh.exists(), "a zero-TTL sweep must reap a dead owner's run dir");
    assert!(live.exists(), "a zero-TTL sweep must still keep a live run dir");

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

    // The launcher's logd binds a per-session socket; under S4 the session dir
    // is the run's private root (`<data>/runs/<id>/socket/logdw`), so find it by
    // name rather than by the old shared `<data>/session/<id>` path.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut logdw = data.join("session/h1spam/socket/logdw");
    while !logdw.exists() {
        if let Ok(rd) = std::fs::read_dir(data.join("runs")) {
            for e in rd.flatten() {
                let cand = e.path().join("socket/logdw");
                if cand.exists() {
                    logdw = cand;
                }
            }
        }
        if logdw.exists() {
            break;
        }
        if Instant::now() >= deadline {
            let tail = std::fs::read_to_string(scratch.join("session.log")).unwrap_or_default();
            panic!("no logd sink under {} within 60 s:\n{tail}", data.join("runs").display());
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
