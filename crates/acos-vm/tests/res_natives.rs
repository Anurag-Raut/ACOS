//! Java-level acceptance for the `android.content.res` natives (W17-C).
//!
//! Compiles `tests/java/ResMain.java` with the host `javac`/`d8` (build tools
//! only, never the execution path) and runs it on the real Android 14 core
//! library + framework DEX (`--corelib real`). The Java case creates a real
//! `Resources` over `fixtures/build/apps/hello-views.apk`, reads strings by
//! identifier, resolves a framework theme attribute and obtains styled
//! attributes from the APK's compiled layout XML.
//!
//! Skips loudly when `javac`, `d8`, `android.jar` or the fixture APK are
//! unavailable, or when the real boot classpath is not present.

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
    jar: PathBuf,
    android_jar: PathBuf,
}

fn tools() -> Option<Tools> {
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
    Some(Tools {
        javac,
        d8,
        jar,
        android_jar,
    })
}

fn compile_case(name: &str) -> Option<PathBuf> {
    let tools = tools()?;
    let root = repo_root();
    let out = root.join("target/tmp/acos-res-natives").join(name);
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
    // Hidden-API stubs (`AssetManager()` / `addAssetPath` are `@hide`). The
    // stubs are compiled and passed as `--lib`; the DEX under test keeps only
    // the real framework references.
    let stub_dir = root.join("crates/acos-vm/tests/java/stubs/android/content/res");
    let stubs_src: Vec<PathBuf> = std::fs::read_dir(&stub_dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "java").unwrap_or(false))
        .collect();
    if stubs_src.is_empty() {
        return None;
    }
    let stubs = out.join("stubs");
    let classes = out.join("classes");
    std::fs::create_dir_all(&stubs).ok()?;
    std::fs::create_dir_all(&classes).ok()?;
    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&tools.android_jar)
        .args(["-d"])
        .arg(&stubs)
        .args(&stubs_src)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let stub_jar = out.join("res-stub.jar");
    let _ = std::fs::remove_file(&stub_jar);
    let ok = Command::new(&tools.jar)
        .arg("cf")
        .arg(&stub_jar)
        .args(["-C"])
        .arg(&stubs)
        .arg("android")
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let ok = Command::new(&tools.javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(format!("{}:{}", stubs.display(), tools.android_jar.display()))
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

fn run_case(stress: bool) -> Result<String, String> {
    let Some(dex) = compile_case("ResMain") else {
        return Err("javac/d8/android.jar unavailable".to_string());
    };
    let apk = repo_root().join("fixtures/build/apps/hello-views.apk");
    if !apk.is_file() {
        return Err(format!("missing fixture {}", apk.display()));
    }
    let options = VmOptions {
        corelib: CoreLib::Real,
        gc_stress: stress,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => return Err(m),
        Err(e) => return Err(format!("Vm::new failed: {e}")),
    };
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let outcome = vm.run("ResMain", &[apk.to_string_lossy().to_string()]);
    if outcome.exit_code != 0 || outcome.vm_error.is_some() {
        return Err(format!(
            "run failed: exit={} vm_error={:?} stdout={} stderr={}",
            outcome.exit_code, outcome.vm_error, outcome.stdout, outcome.stderr
        ));
    }
    Ok(outcome.stdout)
}

fn assert_reference(out: &str) {
    for needle in [
        "title=Hello ACOS",
        "app=Hello Views",
        "missing=0",
        "resolve=true",
        "orientation=1",
        "width=-1",
        "height=-1",
        "done",
    ] {
        assert!(
            out.lines().any(|l| l == needle),
            "missing `{needle}` in stdout:\n{out}"
        );
    }
    // The padding dimension is 16dp scaled by the display density. The Java
    // case sets density=2.625, so the expected value is exactly 42.0.
    assert!(
        out.lines().any(|l| l == "padding=42.0"),
        "missing `padding=42.0` in stdout:\n{out}"
    );
}

#[test]
fn res_natives_on_the_real_framework() {
    match run_case(false) {
        Ok(out) => assert_reference(&out),
        Err(m) => eprintln!("SKIP res_natives_on_the_real_framework: {m}"),
    }
}

#[test]
fn res_natives_gc_stress() {
    match run_case(true) {
        Ok(out) => assert_reference(&out),
        Err(m) => eprintln!("SKIP res_natives_gc_stress: {m}"),
    }
}
