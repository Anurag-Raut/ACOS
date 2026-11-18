//! Regression (W12): a parked looper thread with a second (finished) green
//! thread must not be re-run by the scheduler.
//!
//! The vm-perf rewrite of `interp.rs` dropped `ThreadStatus::Parked` from
//! `run_batch`'s yield condition and `run()` reset the current thread to
//! `Runnable` on every periodic pre-emption. With more than one thread in the
//! VM that resurrected the parked thread forever, burning 100% host CPU and
//! never reaching the `pump` block. This test is the only one in its binary so
//! the process-wide `/proc/self/stat` counters are not perturbed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use acos_dex::DexFile;
use acos_vm::{CoreLib, Vm, VmError, VmOptions};

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

fn sdk_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("ANDROID_HOME") {
        return Some(PathBuf::from(home));
    }
    let sdk = PathBuf::from("/home/Qui/Android/Sdk");
    sdk.is_dir().then_some(sdk)
}

/// Compile `LooperBlockMT.java` (with its anonymous Runnable) to a DEX.
fn compile_block_mt() -> Option<PathBuf> {
    let root = repo_root();
    let out = root.join("target/tmp/acos-java-cases/looper_block_mt");
    let dex = out.join("classes.dex");
    if dex.is_file() {
        return Some(dex);
    }
    let javac = which("javac")?;
    let sdk = sdk_dir()?;
    let d8 = sdk.join("build-tools/35.0.0/d8");
    let android_jar = sdk.join("platforms/android-34/android.jar");
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).ok()?;
    let src = root.join("crates/acos-vm/tests/java/LooperBlockMT.java");
    let status = Command::new(javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&android_jar)
        .arg("-d")
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

/// `utime + stime` from `/proc/self/stat` in clock ticks.
fn cpu_ticks() -> u64 {
    let stat = std::fs::read_to_string("/proc/self/stat").expect("read /proc/self/stat");
    let rest = stat.rsplit_once(')').map(|(_, r)| r).unwrap_or(&stat);
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11).and_then(|v| v.parse().ok()).unwrap_or(0);
    let stime: u64 = fields.get(12).and_then(|v| v.parse().ok()).unwrap_or(0);
    utime + stime
}

#[test]
fn parked_looper_with_second_thread_burns_no_cpu() {
    let Some(dex) = acos_test_support::require_some(compile_block_mt(), "parked_looper_with_second_thread_burns_no_cpu", "javac/d8 unavailable") else {
        return;
    };
    if acos_vm::vm::default_boot_dir(None).is_none() {
        acos_test_support::require_prereq(false, "parked_looper_with_second_thread_burns_no_cpu", "parked_looper_with_second_thread_burns_no_cpu: boot-classpath cache absent");
        return;
    }
    // The VM is not `Send`; build and run it entirely on the helper thread.
    let dex_clone = dex.clone();
    let start = std::sync::Arc::new(std::sync::Barrier::new(2));
    let ready = start.clone();
    thread::spawn(move || {
        let mut vm = match Vm::new(VmOptions {
            corelib: CoreLib::Real,
            ..VmOptions::default()
        }) {
            Ok(vm) => vm,
            Err(VmError::BootClasspathMissing(msg)) => {
                acos_test_support::require_prereq(false, "parked_looper_with_second_thread_burns_no_cpu", &format!("parked_looper_with_second_thread_burns_no_cpu: {msg}"));
                return;
            }
            Err(e) => panic!("Vm::new(CoreLib::Real) failed: {e}"),
        };
        // Keep the run alive long enough to measure; the parked loop must block.
        vm.host = acos_vm::HostGuard::new(0, 0);
        vm.add_dex(DexFile::parse(std::fs::read(&dex_clone).unwrap()).unwrap());
        ready.wait();
        let _ = vm.run("LooperBlockMT", &[]);
    });

    start.wait();
    thread::sleep(Duration::from_secs(3));

    let before = cpu_ticks();
    thread::sleep(Duration::from_secs(5));
    let after = cpu_ticks();

    assert_eq!(
        before, after,
        "parked looper with a second thread consumed CPU ticks ({before} -> {after}); \
         the scheduler is re-running a parked thread"
    );
}
