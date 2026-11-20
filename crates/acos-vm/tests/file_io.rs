//! Java-level acceptance for the ACOS VFS (W17-B).
//!
//! Compiles `tests/java/FileIoMain.java` with the host `javac`/`d8` (build
//! tools only, never the execution path), runs it on the real Android 14 core
//! library, and checks the real `FileInputStream`/`RandomAccessFile`/`File`/`Os`
//! behaviour against the sandboxed VFS:
//!
//! * `/system/etc/fonts.xml` read through `FileInputStream` and
//!   `RandomAccessFile` (seek/length/getFilePointer);
//! * write + read-back in the app files dir (and the `/data/data` alias);
//! * `android.system.ErrnoException` messages for ENOENT / EROFS / EACCES,
//!   rendered by the real `ErrnoException.getMessage()` and `strerror`;
//! * traversal and symlink escapes fail closed.

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
    let out = root.join("target/tmp/acos-file-io").join(name);
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
    let class_file = classes.join(format!("{name}.class"));
    if !class_file.is_file() {
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
fn file_io_on_the_real_framework() {
    let Some(dex) = acos_test_support::require_some(compile_case("FileIoMain"), "file_io_on_the_real_framework", "javac/d8/android.jar unavailable") else {
        return;
    };
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => {
            acos_test_support::require_prereq(false, "file_io_on_the_real_framework", &format!("file_io_on_the_real_framework: {m}"));
            return;
        }
        Err(e) => panic!("Vm::new failed: {e}"),
    };
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let outcome = vm.run("FileIoMain", &[]);
    assert_eq!(
        outcome.exit_code, 0,
        "run failed: vm_error={:?} stderr={}",
        outcome.vm_error, outcome.stderr
    );
    assert!(outcome.vm_error.is_none(), "{:?}", outcome.vm_error);
    let out = parse_stdout(&outcome.stdout);
    let get = |k: &str| -> &str {
        out.get(k)
            .unwrap_or_else(|| panic!("missing `{k}` in stdout:\n{}", outcome.stdout))
            .as_str()
    };

    // 1. FileInputStream reads the boot font config through `/system/etc`.
    assert!(
        get("fonts_head").starts_with("<?xml version=\"1.0\""),
        "fonts_head={}",
        get("fonts_head")
    );

    // 2. RandomAccessFile seek/read/length on the same file.
    assert_eq!(get("raf_tag"), "version", "seek(6) reads from offset 6");
    assert_eq!(get("raf_pos"), "13", "getFilePointer tracks the offset");
    assert_eq!(get("raf_len"), "67396", "fstat st_size");
    assert_eq!(get("raf_first"), "<?xml", "seek(0) rewinds");

    // 3. Write + read-back in the app files dir, and the /data/data alias.
    assert_eq!(
        get("rw_dir"),
        "true",
        "mkdirs created the sandbox files dir"
    );
    assert_eq!(get("rw_same"), "true", "write + read-back bytes match");
    assert_eq!(get("rw_len"), "10", "File.length() after write");
    assert_eq!(
        get("rw_datadata_same"),
        "true",
        "the /data/data alias reads back"
    );

    // 4. ErrnoException messages, exactly as ART renders them.
    //
    // Reference: `jvm/linux-reference` (artifact build of Android 14 +
    // x86_64 Linux UAPI). No emulator was attached when these cases were
    // pinned, so the expected text is the `ErrnoException.getMessage()`
    // contract (`function failed: ENAME (strerror)`) with bionic/glibc
    // `strerror` strings and the real `OsConstants.errnoName` values.
    assert_eq!(get("enoent_errno"), "2");
    assert_eq!(
        get("enoent_msg"),
        "stat failed: ENOENT (No such file or directory)"
    );
    assert_eq!(get("erofs_errno"), "30");
    assert_eq!(
        get("erofs_msg"),
        "open failed: EROFS (Read-only file system)"
    );
    assert_eq!(get("traversal_errno"), "13");
    assert_eq!(
        get("traversal_msg"),
        "stat failed: EACCES (Permission denied)"
    );
    assert_eq!(get("unmapped_errno"), "2");
    assert_eq!(
        get("unmapped_msg"),
        "stat failed: ENOENT (No such file or directory)"
    );

    // 5. A symlink planted in the sandbox cannot escape it.
    assert_eq!(get("readlink"), "/etc/passwd");
    assert_eq!(get("symlink_escape_errno"), "13");
    assert_eq!(
        get("symlink_escape_msg"),
        "stat failed: EACCES (Permission denied)"
    );
    assert!(
        !out.contains_key("enoent_ok") && !out.contains_key("erofs_ok"),
        "a denied operation reported success"
    );
}
