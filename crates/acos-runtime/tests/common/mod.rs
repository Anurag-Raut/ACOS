//! Shared Java-test harness for `acos-runtime` integration tests.
//!
//! Compiles `tests/java/<name>.java` (with the signature-only `@hide` stubs)
//! into a DEX and runs it on the real Android 14 core library + framework DEX.
//! The SDK/`javac`/`d8` toolchain is only used to *build* the test case; it is
//! never on the execution path. When the toolchain or boot-classpath cache is
//! absent the tests skip loudly.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_runtime::Runtime;
use acos_vm::{CoreLib, VmError, VmOptions};

pub fn repo_root() -> PathBuf {
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

pub struct Tools {
    pub javac: PathBuf,
    pub d8: PathBuf,
    pub android_jar: PathBuf,
}

pub fn tools() -> Option<Tools> {
    let javac = which("javac")?;
    let sdk = std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    let d8 = PathBuf::from(format!("{sdk}/build-tools/35.0.0/d8"));
    let android_jar = PathBuf::from(format!("{sdk}/platforms/android-34/android.jar"));
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    Some(Tools {
        javac,
        d8,
        android_jar,
    })
}

fn collect_java(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_java(&path, out);
        } else if path.extension().map(|x| x == "java").unwrap_or(false) {
            out.push(path);
        }
    }
}

fn collect_class_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_class_files(&path, out);
        } else if path.extension().map(|x| x == "class").unwrap_or(false) {
            out.push(path);
        }
    }
}

/// True when `artifact` exists and is at least as new as every dependency.
fn is_fresh(artifact: &Path, deps: &[PathBuf]) -> bool {
    let Ok(a) = std::fs::metadata(artifact) else {
        return false;
    };
    let Ok(at) = a.modified() else { return false };
    deps.iter().all(|d| {
        std::fs::metadata(d)
            .and_then(|m| m.modified())
            .map(|dt| dt <= at)
            .unwrap_or(false)
    })
}

/// Build the signature-only `@hide` stub jar once per test process.
fn stub_jar(tools: &Tools) -> Option<PathBuf> {
    let root = repo_root();
    let out = root.join("target/tmp/acos-runtime/stubs");
    let jar = out.join("hidden-stubs.jar");
    let mut sources = Vec::new();
    collect_java(&root.join("crates/acos-runtime/tests/java/stubs"), &mut sources);
    sources.sort();
    if is_fresh(&jar, &sources) {
        return Some(jar);
    }
    let stubs = out.join("classes");
    std::fs::create_dir_all(&stubs).ok()?;
    if sources.is_empty() {
        return None;
    }
    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&tools.android_jar)
        .arg("-d")
        .arg(&stubs)
        .args(&sources)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let ok = Command::new("jar")
        .arg("cf")
        .arg(&jar)
        .args(["-C"])
        .arg(&stubs)
        .arg(".")
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    Some(jar)
}

/// Compile `tests/java/<name>.java` into one DEX. The real framework classes
/// (and the `@hide` stubs) are `--lib`s for d8 only, never bundled.
pub fn compile_case(names: &[&str]) -> Option<PathBuf> {
    let tools = tools()?;
    let root = repo_root();
    let out = root.join("target/tmp/acos-runtime").join(names.join("-"));
    let dex = out.join("classes.dex");
    let sources: Vec<PathBuf> = names
        .iter()
        .map(|n| root.join("crates/acos-runtime/tests/java").join(format!("{n}.java")))
        .collect();
    let stubs = stub_jar(&tools)?;
    let mut deps = sources.clone();
    deps.push(stubs.clone());
    if is_fresh(&dex, &deps) {
        return Some(dex);
    }
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).ok()?;
    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(format!("{}:{}", stubs.display(), tools.android_jar.display()))
        .arg("-d")
        .arg(&classes)
        .args(&sources)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let mut class_files: Vec<PathBuf> = Vec::new();
    collect_class_files(&classes, &mut class_files);
    class_files.sort();
    if class_files.is_empty() {
        return None;
    }
    let ok = Command::new(&tools.d8)
        .arg("--lib")
        .arg(&tools.android_jar)
        .arg("--lib")
        .arg(&stubs)
        .args(["--min-api", "26", "--output"])
        .arg(&out)
        .args(&class_files)
        .status()
        .ok()?
        .success();
    if !ok || !dex.is_file() {
        return None;
    }
    Some(dex)
}

pub fn new_runtime() -> Option<Runtime> {
    match Runtime::standard_with(VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    }) {
        Ok(rt) => Some(rt),
        Err(VmError::BootClasspathMissing(msg)) => {
            eprintln!("SKIP real-corelib runtime test: {msg}");
            None
        }
        Err(e) => panic!("Runtime::standard_with failed: {e}"),
    }
}

pub fn run_case(dex: &Path, main: &str) -> (Runtime, acos_vm::RunOutcome) {
    let mut rt = new_runtime().expect("runtime");
    rt.vm_mut()
        .add_dex(DexFile::parse(std::fs::read(dex).unwrap()).unwrap());
    let outcome = rt.run(main, &[]);
    (rt, outcome)
}

/// Assert a clean run and return stdout.
pub fn run_ok(dex: &Path, main: &str, label: &str) -> Option<String> {
    let (_rt, outcome) = run_case(dex, main);
    if outcome.vm_error.is_some() || outcome.exit_code != 0 {
        panic!(
            "{label} failed: vm_error={:?}\nstdout:\n{}\nstderr:\n{}",
            outcome.vm_error, outcome.stdout, outcome.stderr
        );
    }
    Some(outcome.stdout)
}
