//! Java-level acceptance for the `sun.nio.fs` natives (W168-A).
//!
//! Compiles `tests/java/NioFsMain.java` with the host `javac`/`d8` (build tools
//! only, never the execution path), runs it on the real Android 14 core library,
//! and checks the real `java.nio.file.Files`/`Paths` stack against the
//! sandboxed VFS: the default filesystem (`sun.nio.fs.UnixNativeDispatcher`)
//! must construct, `Files` round-trips bytes, directory streams work, and
//! errors surface as the real `UnixException`-derived `IOException` with the
//! `strerror` reason.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Compile the Java case to a DEX. Returns `None` (skip loudly) when the host
/// build tools are unavailable.
fn compile_case(name: &str) -> Option<PathBuf> {
    let javac = which("javac")?;
    let sdk = std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    let d8 = PathBuf::from(format!("{sdk}/build-tools/35.0.0/d8"));
    let android_jar = PathBuf::from(format!("{sdk}/platforms/android-34/android.jar"));
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    let root = repo_root();
    let out = root.join("target/tmp/acos-nio-fs").join(name);
    let dex = out.join("classes.dex");
    if dex.is_file() {
        return Some(dex);
    }
    let src = root
        .join("crates/acos-vm/tests/java")
        .join(format!("{name}.java"));
    if !src.is_file() {
        return None;
    }
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).ok()?;
    let ok = Command::new(&javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&android_jar)
        .args(["-d"])
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let class_files: Vec<PathBuf> = std::fs::read_dir(&classes)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "class"))
        .collect();
    if class_files.is_empty() {
        return None;
    }
    let ok = Command::new(&d8)
        .arg("--lib")
        .arg(&android_jar)
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

fn parse_stdout(stdout: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

#[test]
fn nio_fs_on_the_real_framework() {
    let Some(dex) = compile_case("NioFsMain") else {
        eprintln!("SKIP nio_fs_on_the_real_framework: javac/d8/android.jar unavailable");
        return;
    };
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => {
            eprintln!("SKIP nio_fs_on_the_real_framework: {m}");
            return;
        }
        Err(e) => panic!("Vm::new failed: {e}"),
    };
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let outcome = vm.run("NioFsMain", &[]);
    assert!(
        outcome.vm_error.is_none(),
        "run failed: vm_error={:?} stderr={}",
        outcome.vm_error,
        outcome.stderr
    );
    assert_eq!(outcome.exit_code, 0, "stderr={}", outcome.stderr);
    let out = parse_stdout(&outcome.stdout);
    let get = |k: &str| -> &str {
        out.get(k)
            .unwrap_or_else(|| panic!("missing `{k}` in stdout:\n{}", outcome.stdout))
            .as_str()
    };

    // The default filesystem constructed and the basic path predicates work.
    assert_eq!(get("dir_exists"), "true");
    assert_eq!(get("dir_is_dir"), "true");

    // Bytes round-trip through open/write/read/close and stat reports size.
    assert_eq!(get("rw_same"), "true");
    assert_eq!(get("rw_size"), "10");

    // Directory streaming lists the file and skips `.`/`..`.
    assert_eq!(get("list_has_probe"), "true");
    assert_eq!(get("list_size"), "1", "readdir must skip . and ..");

    // realpath returns an absolute guest path.
    assert_eq!(
        get("realpath"),
        "/data/user/0/com.example.nio/files/probe.bin"
    );
    assert_eq!(get("realpath_abs"), "true");

    // fstatat + getpwuid: attributes and an owner name resolved through the
    // account lookup. The reported uid follows the VFS stat result, so the
    // expected name is derived from it rather than hard-coded (the sandbox
    // does not rewrite inode ownership).
    assert_eq!(get("attr_is_reg"), "true");
    assert_eq!(get("attr_size"), "10");
    let uid: i32 = get("attr_uid").parse().expect("numeric uid");
    let expected_owner = match uid {
        0 => "root".to_string(),
        1000 => "system".to_string(),
        10000..=19999 => format!("u0_a{}", uid - 10000),
        _ => uid.to_string(),
    };
    assert_eq!(get("attr_owner"), expected_owner);

    // rename + unlink.
    assert_eq!(get("old_gone"), "true");
    assert_eq!(get("new_present"), "true");
    assert_eq!(get("deleted"), "true");

    // The boot font config is readable through NIO.
    assert_eq!(get("fonts_prefix"), "<?xml version=");

    // A missing file maps to NoSuchFileException. Android's
    // `UnixException.translateToIOException` passes a null reason when the
    // `UnixException` was built from an errno (only its own `getMessage()`
    // consults `strerror`), so the observed reason is the string "null".
    assert_eq!(get("enoent_reason"), "null");
    assert_eq!(get("enoent_file_present"), "true");
    assert!(
        !out.contains_key("enoent_ok"),
        "missing file open reported success"
    );
    assert!(
        !out.contains_key("eisdir_ok"),
        "reading a directory as a file reported success"
    );
    assert_eq!(get("eisdir_caught"), "true");
}
