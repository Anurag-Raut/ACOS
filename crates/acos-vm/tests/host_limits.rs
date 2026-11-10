//! Regression (W12): the host-side ceilings are independent of the guest heap
//! and stop a runaway promptly.
//!
//! * `ACOS_WALL_MAX_SECS` aborts a non-allocating infinite loop with `TIMEOUT`.
//! * `ACOS_HOST_RSS_MAX` aborts the first allocation with `HOST_MEMORY_LIMIT`.
//!
//! Both tests bound themselves: the VM runs on a helper thread and the test
//! waits on a channel with a deadline, so a broken guard fails instead of
//! hanging `cargo test`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use acos_dex::DexFile;
use acos_vm::{HostGuard, Vm, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn compile_busy_loop() -> Option<PathBuf> {
    let root = repo_root();
    let out = root.join("target/tmp/acos-java-cases/host_limits");
    let dex = out.join("classes.dex");
    if dex.is_file() {
        return Some(dex);
    }
    let javac = which("javac")?;
    let sdk = std::env::var("ANDROID_HOME").unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    let d8 = PathBuf::from(format!("{sdk}/build-tools/35.0.0/d8"));
    let android_jar = PathBuf::from(format!("{sdk}/platforms/android-34/android.jar"));
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    let src = root.join("crates/acos-vm/tests/java/BusyLoopMain.java");
    if !src.is_file() {
        return None;
    }
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).ok()?;
    let status = Command::new(javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-d"])
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let mut class_files: Vec<PathBuf> = std::fs::read_dir(&classes)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "class").unwrap_or(false))
        .collect();
    class_files.sort();
    let status = Command::new(d8)
        .arg("--lib")
        .arg(&android_jar)
        .args(["--min-api", "26", "--output"])
        .arg(&out)
        .args(&class_files)
        .status()
        .ok()?;
    if !status.success() || !dex.is_file() {
        return None;
    }
    Some(dex)
}

/// Run `BusyLoopMain` with the given host guard on a helper thread. Returns the
/// structured VM error code (or `None` if it somehow finished). Fails the test
/// if the VM does not return within `deadline`.
fn run_with_guard(dex: &Path, guard: HostGuard, deadline: Duration) -> Option<String> {
    let dex = dex.to_path_buf();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut vm = Vm::new(VmOptions::default()).expect("bootstrap VM");
        vm.host = guard;
        vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
        let outcome = vm.run("BusyLoopMain", &[]);
        let code = outcome.vm_error.map(|e| e.code().to_string());
        let _ = tx.send(code);
    });
    match rx.recv_timeout(deadline) {
        Ok(code) => code,
        Err(_) => panic!("VM did not return within {deadline:?}; host guard failed to abort"),
    }
}

#[test]
fn wall_watchdog_aborts_infinite_loop() {
    let Some(dex) = compile_busy_loop() else {
        eprintln!("SKIP wall_watchdog_aborts_infinite_loop: javac/d8 unavailable");
        return;
    };
    let code = run_with_guard(&dex, HostGuard::new(0, 1), Duration::from_secs(15));
    assert_eq!(
        code.as_deref(),
        Some("TIMEOUT"),
        "expected TIMEOUT from a non-allocating infinite loop"
    );
}

#[test]
fn host_rss_ceiling_aborts_allocation() {
    let Some(dex) = compile_busy_loop() else {
        eprintln!("SKIP host_rss_ceiling_aborts_allocation: javac/d8 unavailable");
        return;
    };
    // A one-byte ceiling is below any live process RSS, so the first allocation
    // must abort. This proves the ceiling is host-side, not `ACOS_HEAP_MAX`.
    let code = run_with_guard(&dex, HostGuard::new(1, 0), Duration::from_secs(15));
    assert_eq!(
        code.as_deref(),
        Some("HOST_MEMORY_LIMIT"),
        "expected HOST_MEMORY_LIMIT from a 1-byte host RSS ceiling"
    );
}
