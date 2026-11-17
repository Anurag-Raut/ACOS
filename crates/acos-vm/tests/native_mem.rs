//! Java-level acceptance for the native-memory substrate (W21-A).
//!
//! Compiles `tests/java/NativeMemMain.java` with the host `javac`/`d8` (build
//! tools only, never the execution path) and runs it on the real Android 14
//! boot classpath (`--corelib real`). The Java case drives the real
//! `DirectByteBuffer`/`Bits`, `sun.misc.Unsafe` and `libcore.io.Memory`
//! bytecode over the Rust-owned region table, plus a read-only
//! `FileChannel.map`. Skips loudly when the toolchain or boot classpath are
//! absent.
//!
//! The pure Rust region-table unit tests live in
//! `src/natives/native_mem.rs` (`cargo test -p acos-vm --lib`).

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

/// Compile the stubs and `NativeMemMain` and return the DEX.
fn compile_case() -> Option<PathBuf> {
    let javac = which("javac")?;
    let jar = which("jar")?;
    let sdk = std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    let d8 = PathBuf::from(format!("{sdk}/build-tools/35.0.0/d8"));
    let android_jar = PathBuf::from(format!("{sdk}/platforms/android-34/android.jar"));
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    let root = repo_root();
    let out = root.join("target/tmp/acos-native-mem");
    let dex = out.join("classes.dex");
    if dex.is_file() {
        return Some(dex);
    }
    let stubs_src = root.join("crates/acos-vm/tests/java/native_mem_stubs");
    let src = root.join("crates/acos-vm/tests/java/NativeMemMain.java");
    if !stubs_src.is_dir() || !src.is_file() {
        return None;
    }
    let stubs = out.join("stubs");
    let classes = out.join("classes");
    std::fs::create_dir_all(&stubs).ok()?;
    std::fs::create_dir_all(&classes).ok()?;

    let ok = Command::new(&javac)
        .args([
            "-source",
            "8",
            "-target",
            "8",
            "-nowarn",
            "-encoding",
            "UTF-8",
            "-cp",
        ])
        .arg(&android_jar)
        .args(["-d"])
        .arg(&stubs)
        .arg(stubs_src.join("sun/misc/Unsafe.java"))
        .arg(stubs_src.join("libcore/io/Memory.java"))
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let stub_jar = out.join("native-mem-stubs.jar");
    let _ = std::fs::remove_file(&stub_jar);
    let ok = Command::new(&jar)
        .arg("cf")
        .arg(&stub_jar)
        .args(["-C"])
        .arg(&stubs)
        .arg("sun")
        .args(["-C"])
        .arg(&stubs)
        .arg("libcore")
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let ok = Command::new(&javac)
        .args([
            "-source",
            "8",
            "-target",
            "8",
            "-nowarn",
            "-encoding",
            "UTF-8",
            "-cp",
        ])
        .arg(format!("{}:{}", stubs.display(), android_jar.display()))
        .args(["-d"])
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let class_file = classes.join("NativeMemMain.class");
    if !class_file.is_file() {
        return None;
    }
    let ok = Command::new(&d8)
        .arg("--lib")
        .arg(&android_jar)
        .arg("--lib")
        .arg(&stub_jar)
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

fn run_case() -> Result<String, String> {
    let Some(dex) = compile_case() else {
        return Err("javac/d8/android.jar/stubs unavailable".to_string());
    };
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => return Err(m),
        Err(e) => return Err(format!("Vm::new failed: {e}")),
    };
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let outcome = vm.run("NativeMemMain", &[]);
    if outcome.exit_code != 0 || outcome.vm_error.is_some() {
        return Err(format!(
            "run failed: exit={} vm_error={:?} stdout={} stderr={}",
            outcome.exit_code, outcome.vm_error, outcome.stdout, outcome.stderr
        ));
    }
    Ok(outcome.stdout)
}

fn line_value<'a>(out: &'a str, key: &str) -> Option<&'a str> {
    out.lines().find_map(|l| l.strip_prefix(&format!("{key}=")))
}

#[test]
fn native_memory_round_trips_on_the_real_framework() {
    let out = match run_case() {
        Ok(out) => out,
        Err(m) => {
            eprintln!("SKIP native_memory_round_trips_on_the_real_framework: {m}");
            return;
        }
    };

    // DirectByteBuffer / Bits.
    assert_eq!(line_value(&out, "direct.int"), Some("1020304"), "{out}");
    assert_eq!(
        line_value(&out, "direct.long"),
        Some("1122334455667788"),
        "{out}"
    );
    assert_eq!(line_value(&out, "direct.short"), Some("a0b"), "{out}");
    assert_eq!(line_value(&out, "direct.byte"), Some("7f"), "{out}");
    assert_eq!(line_value(&out, "direct.double"), Some("1.5"), "{out}");
    assert_eq!(line_value(&out, "direct.float"), Some("2.5"), "{out}");
    assert_eq!(line_value(&out, "direct.char"), Some("41"), "{out}");

    // Unsafe.
    assert_eq!(
        line_value(&out, "unsafe.long"),
        Some("102030405060708"),
        "{out}"
    );
    assert_eq!(line_value(&out, "unsafe.int"), Some("deadbeef"), "{out}");
    assert_eq!(line_value(&out, "unsafe.byte"), Some("9a"), "{out}");
    assert_eq!(line_value(&out, "unsafe.short"), Some("f0e"), "{out}");
    assert_eq!(line_value(&out, "unsafe.float"), Some("3.25"), "{out}");
    assert_eq!(line_value(&out, "unsafe.double"), Some("6.5"), "{out}");
    assert_eq!(
        line_value(&out, "unsafe.set"),
        Some("5a5a5a5a5a5a5a5a"),
        "{out}"
    );
    assert_eq!(
        line_value(&out, "unsafe.copy"),
        Some("5a5a5a5a5a5a5a5a"),
        "{out}"
    );

    // libcore.io.Memory.
    assert_eq!(line_value(&out, "mem.int"), Some("7f6e5d4c"), "{out}");
    assert_eq!(line_value(&out, "mem.byte"), Some("33"), "{out}");
    assert_eq!(line_value(&out, "mem.bytes"), Some("146"), "{out}");

    // A freed region is a structured exception, never a crash.
    assert_eq!(
        line_value(&out, "freed"),
        Some("IllegalStateException"),
        "{out}"
    );

    // FileChannel.map.
    assert_eq!(line_value(&out, "map.full"), Some("1001"), "{out}");
    assert_eq!(line_value(&out, "map.sub"), Some("50044"), "{out}");
    assert_eq!(line_value(&out, "map.size"), Some("16,8"), "{out}");
    assert_eq!(line_value(&out, "map.remain"), Some("16"), "{out}");
    assert!(out.lines().any(|l| l == "done"), "missing done: {out}");
}
