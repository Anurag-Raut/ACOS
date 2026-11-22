//! Java-level acceptance for the `java.io.UnixFileSystem` natives (W181-A).
//!
//! Compiles `tests/java/JavaFileMain.java` with the host `javac` (build tool
//! only, never the execution path) and runs the *same* class twice:
//!
//! 1. on the host JVM — real OpenJDK `java.io.UnixFileSystem`, a real
//!    filesystem, i.e. "real Java" as the oracle;
//! 2. on ACOS — the real Android 14 core library over the sandboxed VFS.
//!
//! Every printed `key=value` line is path- and environment-independent, so the
//! two runs must agree verbatim; that comparison is the acceptance criterion
//! for `createDirectory`, `createFileExclusively`, `getLength`,
//! `checkAccess`, `setPermission`, `setReadOnly`, `get(Set)LastModifiedTime`,
//! `rename0`, `delete0`, `list` and `getSpace`.
//!
//! Split outputs, not shared assertions: a mismatch names the exact native
//! (`key`) and both values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_vm::{CoreLib, Vm, VmError, VmOptions};

/// Guest path handed to the Java case; the VFS maps it to the sandbox root.
const GUEST_BASE: &str = "/data/user/0/com.example.file/files";

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

fn android_jar() -> PathBuf {
    let sdk = std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    PathBuf::from(format!("{sdk}/platforms/android-34/android.jar"))
}

/// Compile the case once with the host `javac` and package it as a DEX.
///
/// The `.class` output is shared by the host-JVM run (real Java oracle) and the
/// `d8` step (ACOS run), which is what makes the two runs the same program.
/// Returns `None` (skip loudly) when the host build tools are unavailable.
fn compile_case() -> Option<(PathBuf, PathBuf)> {
    let javac = which("javac")?;
    let sdk = std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    let d8 = PathBuf::from(format!("{sdk}/build-tools/35.0.0/d8"));
    let android_jar = android_jar();
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    let out = repo_root().join("target/tmp/acos-io-file");
    let classes = out.join("classes");
    let dex = out.join("classes.dex");
    std::fs::create_dir_all(&classes).ok()?;
    let src = repo_root().join("crates/acos-vm/tests/java/JavaFileMain.java");
    if !src.is_file() {
        return None;
    }
    // Rebuild when the source is newer than the compiled class.
    let class_file = classes.join("JavaFileMain.class");
    let stale = !class_file.is_file()
        || class_file.metadata().ok()?.modified().ok()? < src.metadata().ok()?.modified().ok()?;
    if stale || !dex.is_file() {
        let ok = Command::new(&javac)
            .args(["--release", "11", "-encoding", "UTF-8", "-d"])
            .arg(&classes)
            .arg(&src)
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
        let ok = Command::new(&d8)
            .arg("--lib")
            .arg(&android_jar)
            .args(["--min-api", "26", "--output"])
            .arg(&out)
            .arg(&class_file)
            .status()
            .ok()?
            .success();
        if !ok || !dex.is_file() {
            return None;
        }
    }
    Some((classes, dex))
}

fn parse_stdout(stdout: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

/// Run the compiled class on the host JVM (real Java oracle).
fn run_on_host_java(classes: &Path, base: &Path) -> Option<BTreeMap<String, String>> {
    let java = which("java")?;
    let out = Command::new(java)
        .arg("-cp")
        .arg(classes)
        .arg("JavaFileMain")
        .arg(base)
        .output()
        .ok()?;
    if !out.status.success() {
        panic!(
            "host java run failed: status={:?}\nstderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Some(parse_stdout(&String::from_utf8_lossy(&out.stdout)))
}

#[test]
fn unix_file_system_matches_real_java() {
    let Some((classes, dex)) = acos_test_support::require_some(compile_case(), "unix_file_system_matches_real_java", "javac/d8/android.jar unavailable") else {
        return;
    };

    // Oracle: the same class on the host JVM over a throwaway host directory.
    let host_base = repo_root().join("target/tmp/acos-io-file/host-base");
    let _ = std::fs::remove_dir_all(&host_base);
    std::fs::create_dir_all(&host_base).unwrap();
    let Some(host) = acos_test_support::require_some(run_on_host_java(&classes, &host_base), "unix_file_system_matches_real_java", "host java unavailable") else {
        return;
    };

    // ACOS: the real Android 14 core library over the VFS.
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => {
            acos_test_support::require_prereq(false, "unix_file_system_matches_real_java", &format!("unix_file_system_matches_real_java: {m}"));
            return;
        }
        Err(e) => panic!("Vm::new failed: {e}"),
    };
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let outcome = vm.run("JavaFileMain", &[GUEST_BASE.to_string()]);
    assert!(
        outcome.vm_error.is_none(),
        "run failed: vm_error={:?} stderr={}",
        outcome.vm_error,
        outcome.stderr
    );
    assert_eq!(outcome.exit_code, 0, "stderr={}", outcome.stderr);
    let acos = parse_stdout(&outcome.stdout);
    assert!(
        !acos.is_empty(),
        "no key=value output from the ACOS run:\nstdout={}\nstderr={}",
        outcome.stdout,
        outcome.stderr
    );

    // The strongest form: identical key sets and identical values.
    let mismatches: Vec<String> = host
        .iter()
        .filter_map(|(k, hv)| match acos.get(k) {
            Some(av) if av == hv => None,
            Some(av) => Some(format!("{k}: real Java={hv} acos={av}")),
            None => Some(format!("{k}: real Java={hv} acos=<missing>")),
        })
        .chain(
            acos.keys()
                .filter(|k| !host.contains_key(*k))
                .map(|k| format!("{k}: real Java=<missing> acos={}", acos[k])),
        )
        .collect();
    assert!(
        mismatches.is_empty(),
        "ACOS differs from real Java:\n{}\n--- real Java ---\n{}\n--- ACOS ---\n{}",
        mismatches.join("\n"),
        outcome.stdout,
        outcome.stdout
    );

    // Spot-check the values that pin the new natives (so a hypothetical
    // host-JVM drift cannot silently weaken the test).
    let get = |k: &str| -> &str {
        acos.get(k)
            .unwrap_or_else(|| panic!("missing `{k}` in stdout:\n{}", outcome.stdout))
            .as_str()
    };
    assert_eq!(get("mkdir"), "true");
    assert_eq!(get("create_new"), "true");
    assert_eq!(get("create_again"), "false");
    assert_eq!(get("file_length_after"), "7", "getLength");
    assert_eq!(get("can_write_after_readonly"), "false", "setReadOnly");
    assert_eq!(get("can_write_restored"), "true", "setPermission(W)");
    assert_eq!(get("can_execute_set"), "true", "setPermission(X)");
    assert_eq!(get("can_execute_cleared"), "false", "setPermission(!X)");
    assert_eq!(
        get("last_modified"),
        "1600000000000",
        "get/setLastModifiedTime"
    );
    assert_eq!(get("rename"), "true", "rename0");
    assert_eq!(get("old_exists"), "false");
    assert_eq!(get("delete"), "true", "delete0");
    assert_eq!(get("delete_again"), "false");
    assert_eq!(get("hidden_is_hidden"), "true");
    assert_eq!(get("list"), ".hidden");
    assert_eq!(get("list_len"), "1");
    assert_eq!(get("free_space_positive"), "true", "getSpace");
    assert_eq!(get("missing_exists"), "false", "checkAccess/stat");
    assert_eq!(get("missing_length"), "0");
    assert_eq!(get("missing_delete"), "false");
    assert_eq!(get("missing_rename"), "false");
    let _ = android_jar();
}
