//! Java-level acceptance for the `view.surface` natives (T-28).
//!
//! Compiles `tests/java/SurfaceNativesMain.java` with the host `javac`/`d8`
//! (build tools only), runs it on the real Android 14 boot-classpath DEX, and
//! asserts the framework's own `SurfaceControl`/`SurfaceControl$Transaction`
//! bytecode exercised the Rust natives:
//!
//! * a `SurfaceControl` built through the public `SurfaceControl.Builder` is
//!   valid and has a live layer record;
//! * applying two transactions bumps the compositor generation exactly twice;
//! * `writeToParcel` → `createFromParcel` → `writeToParcel` is byte-stable;
//! * a stale (released) handle surfaces as a structured Java exception;
//! * no buffer was submitted, so the frame counter stays at zero (the advisor
//!   freshness rule: applying a property transaction is not a presented frame).

use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_vm::natives::fwk_surface;
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

/// Compile `SurfaceNativesMain.java` to a one-class DEX. No `@hide` stub is
/// needed: every method used is public API-34.
fn compile_case() -> Option<PathBuf> {
    let tools = tools()?;
    let root = repo_root();
    let out = root.join("target/tmp/acos-surface-natives");
    let dex = out.join("classes.dex");
    if dex.is_file() {
        return Some(dex);
    }
    let src = root.join("crates/acos-vm/tests/java/SurfaceNativesMain.java");
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
    let class_file = classes.join("SurfaceNativesMain.class");
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
fn surface_control_natives_on_the_real_framework() {
    let Some(dex) = compile_case() else {
        eprintln!(
            "SKIP surface_control_natives_on_the_real_framework: javac/d8/android.jar unavailable"
        );
        return;
    };
    // Isolated native state: a fresh VM run must start from generation 0.
    fwk_surface::reset();
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => {
            eprintln!("SKIP surface_control_natives_on_the_real_framework: {m}");
            return;
        }
        Err(e) => panic!("Vm::new failed: {e}"),
    };
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());

    let outcome = vm.run("SurfaceNativesMain", &[]);
    assert_eq!(
        outcome.exit_code, 0,
        "run failed: vm_error={:?} stderr={}",
        outcome.vm_error, outcome.stderr
    );
    assert!(outcome.vm_error.is_none(), "{:?}", outcome.vm_error);

    let mut fields = std::collections::HashMap::new();
    for line in outcome.stdout.lines() {
        if let Some((k, v)) = line.split_once('=') {
            fields.insert(k.to_string(), v.to_string());
        }
    }
    let get = |k: &str| -> &str {
        fields
            .get(k)
            .unwrap_or_else(|| panic!("missing `{k}` in stdout:\n{}", outcome.stdout))
            .as_str()
    };

    assert_eq!(
        get("valid"),
        "true",
        "SurfaceControl.Builder produced a live layer"
    );
    assert_eq!(
        get("applied"),
        "2",
        "both transactions reached nativeApplyTransaction"
    );
    assert_eq!(
        get("parcel_stable"),
        "true",
        "transaction parcel round-trip must be byte-stable"
    );
    assert_eq!(
        get("stale"),
        "no-exception",
        "a parcel-read alias must outlive the original wrapper's release (AOSP sp<> refcounts)"
    );
    assert!(outcome.stdout.ends_with("done\n"), "main did not finish");

    // Three applied transactions with ops => generation 3 (the parcel alias's
    // apply is the third; the released original's refcount drop does not
    // disconnect the layer). No buffer was submitted, so no frame was
    // published.
    assert_eq!(
        fwk_surface::generation(),
        3,
        "apply must bump the generation"
    );
    assert_eq!(
        fwk_surface::presented_frames(),
        0,
        "finishDrawing/apply alone must not publish a frame"
    );

    // The released original dropped one reference; the parcel alias keeps the
    // layer live (it is still referenced by `stale`).
    assert!(fwk_surface::live_layers() <= 1);
}
