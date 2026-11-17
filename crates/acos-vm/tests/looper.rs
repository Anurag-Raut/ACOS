//! `android.os.MessageQueue` natives: real-framework looper integration plus
//! direct Rust tests of the epoll/eventfd looper primitives.
//!
//! The Java-level tests compile `tests/java/LooperMain.java` with the host
//! `javac`/`d8` (build tools only, never the execution path) and run it on the
//! **real Android 14 boot classpath** (`--corelib real`). They skip loudly when
//! the SDK or the boot-classpath cache is absent.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use acos_dex::DexFile;
use acos_vm::natives::looper::{create_looper, destroy_looper};
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

/// Compile `tests/java/<names>.java` to a single DEX. Returns `None` (with a
/// loud skip message) when the host build tools are unavailable.
fn compile_java(names: &[&str]) -> Option<PathBuf> {
    let root = repo_root();
    // Key the output on the source set so two tests compiling different
    // classes do not collide on one cached `classes.dex`.
    let out = root.join(format!(
        "target/tmp/acos-java-cases/looper-{}",
        names.join("-")
    ));
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
    let sources: Vec<PathBuf> = names
        .iter()
        .map(|n| {
            root.join("crates/acos-vm/tests/java")
                .join(format!("{n}.java"))
        })
        .collect();
    let status = Command::new(javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&android_jar)
        .arg("-d")
        .arg(&classes)
        .args(&sources)
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

/// One host `javac`/`d8` compile, run once per process via a cache file.
fn new_real_vm() -> Option<Vm> {
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    match Vm::new(options) {
        Ok(vm) => Some(vm),
        Err(VmError::BootClasspathMissing(msg)) => {
            eprintln!("SKIP real-corelib looper test: {msg}");
            None
        }
        Err(e) => panic!("Vm::new(CoreLib::Real) failed: {e}"),
    }
}

fn run_real(dex: &Path, main: &str) -> Option<(i32, String, Option<VmError>)> {
    let mut vm = new_real_vm()?;
    vm.add_dex(DexFile::parse(std::fs::read(dex).unwrap()).unwrap());
    let outcome = vm.run(main, &[]);
    Some((outcome.exit_code, outcome.stdout, outcome.vm_error))
}

#[test]
fn real_looper_delayed_and_green_thread_wake() {
    let Some(dex) = compile_java(&["LooperMain"]) else {
        eprintln!("SKIP real_looper_delayed_and_green_thread_wake: javac/d8 unavailable");
        return;
    };
    let Some((code, stdout, err)) = run_real(&dex, "LooperMain") else {
        return;
    };
    assert_eq!(code, 0, "run failed: {err:?}");
    assert!(
        stdout.contains("woke_from_green_thread"),
        "green-thread wake missing: {stdout:?}"
    );
    let delayed: i64 = stdout
        .lines()
        .find_map(|l| l.strip_prefix("delayed_ms="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("delayed line missing: {stdout:?}"));
    assert!(
        (98..=110).contains(&delayed),
        "postDelayed fired at {delayed} ms (want ~100, +/-2 plus scheduling slack)"
    );
    // The green-thread message (30 ms) must precede the 100 ms delayed one.
    assert!(
        stdout.find("woke_from_green_thread").unwrap() < stdout.find("delayed_ms=").unwrap(),
        "message ordering wrong: {stdout:?}"
    );
}

#[test]
fn looper_handle_wakes_from_host_thread() {
    let handle = create_looper().expect("create looper");
    let id = handle.id();
    let waker = handle.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        waker.wake().expect("wake");
    });
    let start = Instant::now();
    let woken = handle.wait(None);
    let elapsed = start.elapsed();
    destroy_looper(id);
    assert!(woken, "wait must report an event");
    assert!(
        elapsed >= Duration::from_millis(40),
        "woke too early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "woke too late: {elapsed:?}"
    );
}

/// Virtual time: a 60 s delayed message must fire in well under a second of
/// wall time, and the guest must still observe the full 60 s of uptime.
#[test]
fn virtual_looper_advances_clock_to_future_deadline() {
    let Some(dex) = compile_java(&["LooperVirtual"]) else {
        eprintln!("SKIP virtual_looper_advances_clock_to_future_deadline: javac/d8 unavailable");
        return;
    };
    let Some(mut vm) = new_real_vm() else {
        return;
    };
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let start = Instant::now();
    let outcome = vm.run_virtual("LooperVirtual", &[]);
    let wall = start.elapsed();
    assert_eq!(
        outcome.exit_code, 0,
        "virtual run failed: {:?}\n{}",
        outcome.vm_error, outcome.stderr
    );
    let delayed: i64 = outcome
        .stdout
        .lines()
        .find_map(|l| l.strip_prefix("virtual_delayed_ms="))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or_else(|| panic!("delayed line missing: {:?}", outcome.stdout));
    assert!(
        (59_990..=60_010).contains(&delayed),
        "guest observed {delayed} ms, want ~60000"
    );
    assert!(
        wall < Duration::from_secs(5),
        "virtual time must not sleep for the deadline: {wall:?}"
    );
}

#[test]
fn looper_wait_timeout_returns_false() {
    let handle = create_looper().expect("create looper");
    let start = Instant::now();
    let woken = handle.wait(Some(Duration::from_millis(30)));
    let elapsed = start.elapsed();
    destroy_looper(handle.id());
    assert!(!woken, "no wake was posted");
    assert!(
        elapsed >= Duration::from_millis(20),
        "returned before the timeout: {elapsed:?}"
    );
}
