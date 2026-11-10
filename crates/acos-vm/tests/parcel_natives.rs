//! Java-level acceptance for the `android.os.Parcel` natives (T-18).
//!
//! Compiles `tests/java/ParcelMain.java` with the host `javac`/`d8` (build
//! tools only, never the execution path) and runs it on the real Android 14
//! core library + framework DEX (`--corelib real`). The Java case asserts the
//! full round-trip set required by T-18:
//!
//! * int/long/float/double, String (String16), String[] (String16[]), byte[]
//!   written through `Parcel.obtain`, read back after `setDataPosition(0)`;
//! * a `Bundle` carrying int + String + an `Integer` `ArrayList`;
//! * a `List` through `writeList`/`readList`;
//! * `appendFrom`;
//! * `marshall`/`unmarshall`;
//! * the interface-token magic is `'SYST'` (`0x53595354`).
//!
//! Skips loudly when `javac`, `d8` or `android.jar` are unavailable, or when the
//! real boot classpath is not present.

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

struct Tools {
    javac: PathBuf,
    d8: PathBuf,
    android_jar: PathBuf,
}

fn tools() -> Option<Tools> {
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

/// Compile the Java case into a DEX that references the real framework classes
/// (they are only `--lib`s for d8, never bundled into the DEX).
fn compile_case(name: &str) -> Option<PathBuf> {
    let tools = tools()?;
    let root = repo_root();
    let out = root.join("target/tmp/acos-parcel-natives").join(name);
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
    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&tools.android_jar)
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
    let ok = Command::new(&tools.d8)
        .arg("--lib")
        .arg(&tools.android_jar)
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

#[test]
fn parcel_round_trips_on_the_real_framework() {
    let Some(dex) = compile_case("ParcelMain") else {
        eprintln!("SKIP parcel_round_trips_on_the_real_framework: javac/d8/android.jar unavailable");
        return;
    };
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => {
            eprintln!("SKIP parcel_round_trips_on_the_real_framework: {m}");
            return;
        }
        Err(e) => panic!("Vm::new failed: {e}"),
    };
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let outcome = vm.run("ParcelMain", &[]);
    assert_eq!(
        outcome.exit_code, 0,
        "run failed: vm_error={:?} stdout={} stderr={}",
        outcome.vm_error, outcome.stdout, outcome.stderr
    );
    assert!(outcome.vm_error.is_none(), "{:?}", outcome.vm_error);

    for needle in [
        "primitives=ok",
        "bundle=7,hi,[10,20]",
        "list=1,two,true",
        "append=111,one",
        "marshall=ok",
        "fd=ok",
        "token-header=0x53595354",
    ] {
        assert!(
            outcome.stdout.contains(needle),
            "missing `{needle}` in stdout:\n{}",
            outcome.stdout
        );
    }
    assert!(outcome.stdout.ends_with("done\n"), "main did not finish");
}
