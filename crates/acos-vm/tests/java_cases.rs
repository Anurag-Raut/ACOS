//! JVM-referenced edge-case tests that need a real DEX. The Java sources live
//! in `tests/java/`; this test compiles them with the host `javac`/`d8` (build
//! tools only, never the execution path) and skips loudly if they are absent.

use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_vm::{Vm, VmError, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn compile_case(name: &str) -> Option<PathBuf> {
    let root = repo_root();
    let out = root.join("target/tmp/acos-java-cases").join(name);
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
    let src = root
        .join("crates/acos-vm/tests/java")
        .join(format!("{name}.java"));
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

fn run(dex: &Path, main: &str, heap_max: usize) -> (i32, String, Option<VmError>) {
    let mut vm = Vm::new(VmOptions {
        heap_max_bytes: heap_max,
        gc_stress: false,
    })
    .unwrap();
    vm.add_dex(DexFile::parse(std::fs::read(dex).unwrap()).unwrap());
    let outcome = vm.run(main, &[]);
    (outcome.exit_code, outcome.stdout, outcome.vm_error)
}

#[test]
fn stack_overflow_is_catchable() {
    let Some(dex) = compile_case("StackOverflowMain") else {
        eprintln!("SKIP stack_overflow_is_catchable: javac/d8 unavailable");
        return;
    };
    let (code, stdout, _) = run(&dex, "StackOverflowMain", 256 * 1024 * 1024);
    assert_eq!(code, 0);
    assert_eq!(stdout, "caught-stack-overflow\ndone\n");
}

#[test]
fn out_of_memory_is_catchable_with_small_heap() {
    let Some(dex) = compile_case("OomMain") else {
        eprintln!("SKIP out_of_memory_is_catchable_with_small_heap: javac/d8 unavailable");
        return;
    };
    let (code, stdout, _) = run(&dex, "OomMain", 4 * 1024 * 1024);
    assert_eq!(code, 0);
    assert_eq!(stdout, "caught-oom\ndone\n");
}

#[test]
fn unsupported_java_api_is_structured() {
    let Some(dex) = compile_case("UnsupportedMain") else {
        eprintln!("SKIP unsupported_java_api_is_structured: javac/d8 unavailable");
        return;
    };
    let (code, _stdout, err) = run(&dex, "UnsupportedMain", 256 * 1024 * 1024);
    assert_eq!(code, 70, "unsupported run must not succeed");
    match err {
        Some(VmError::UnsupportedApi { api, at }) => {
            assert!(
                api.contains("java/util/concurrent/Executors"),
                "api was {api}"
            );
            assert!(!at.is_empty(), "call site must be reported, got empty");
        }
        other => panic!("expected UNSUPPORTED_JAVA_API, got {other:?}"),
    }
}
